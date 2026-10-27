//! The Settings window and its accounts panel.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{self, AccountConfig};
use crate::i18n::i18n;
use crate::ui::accounts::{AccountsOutput, AccountsWindow};
use crate::ui::preferences::{PrefInit, PrefInput, PrefOutput, Preferences};

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use crate::ui::dialog::confirm_destructive;

/// What the Settings window reports, as the message it means here.
fn prefs_output_msg(out: PrefOutput) -> AppMsg {
    match out {
        PrefOutput::SetReaderSwitch(on) => AppMsg::SetReaderSwitchShown(on),
        PrefOutput::SetThemeSwitch(on) => AppMsg::SetThemeSwitchShown(on),
        PrefOutput::SetReaderZoom(z) => AppMsg::SetZoomDefault(z),
        PrefOutput::PageShown(id) => AppMsg::SettingsPageShown(id),
        PrefOutput::Closed => AppMsg::ClosePreferences,
        // Every other output is a setting, which AppMsg::Pref applies.
        other => AppMsg::Pref(other),
    }
}

impl AppModel {
    /// Build the Settings window and its accounts panel, hidden. It is
    /// kept for the life of the app once built: `open_settings_window`
    /// presents it, and closing it hides it, so opening never waits on
    /// hundreds of rows being built and laid out again.
    fn build_settings_window(&mut self, sender: &ComponentSender<Self>, on_accounts: bool) {
        let t_open = std::time::Instant::now();
        // Pass accounts in display order. Passwords stay in the keyring until
        // an account's editor opens (the panel fetches them then, off the
        // main thread): reading every account's secrets up front cost a
        // keyring round trip or three per account, which is what made this
        // window slow to appear.
        let init = self.accounts_panel_init();
        self.accounts_seed = Some(format!("{init:?}"));
        let accounts = Self::launch_accounts_panel(init, sender);
        let init = self.pref_init(&accounts, on_accounts);
        let prefs = Preferences::builder()
            .transient_for(&self.window)
            .launch(init)
            .forward(sender.input_sender(), prefs_output_msg);
        accounts.emit(crate::ui::accounts::AccountsInput::SetFolderChoices(
            self.folder_choice_map(),
        ));
        self.arm_settings_showcase(&accounts, sender);
        self.accounts_win = Some(accounts);
        tracing::debug!("settings window: built in {:?}", t_open.elapsed());
        self.prefs = Some(prefs);
    }

    /// The Settings window's first values: every setting as it stands.
    fn pref_init(&self, accounts: &Controller<AccountsWindow>, on_accounts: bool) -> PrefInit {
        PrefInit {
            auto_remote_content: self.auto_remote_content,
            show_remote_banner: self.show_remote_banner,
            show_spoof_banner: self.show_spoof_banner,
            gravatar: self.gravatar,
            avatars: self.avatars,
            own_mailbox_face: self.own_mailbox_face,
            tag_row_tint: self.tag_row_tint,
            sender_logos: self.sender_logos,
            date_style: self.date_style,
            clock_style: self.clock_style,
            language: config::load_language(),
            fetch_interval_secs: self.fetch_interval_secs,
            push: self.push,
            metered_save_data: self.metered_save_data,
            palette_collapse_secs: self.palette_collapse_secs,
            card_palette_collapse_secs: self.card_palette_collapse_secs,
            threading: self.threading,
            threads_expanded: self.threads_expanded,
            thread_newest_first: self.thread_newest_first,
            fold_messages: self.fold_messages,
            always_show_recipients: self.always_show_recipients,
            pgp_labels: self.pgp_labels,
            reader_find_bottom: self.reader_find_bottom,
            single_message_card: self.single_message_card,
            reader_switch: self.reader_switch,
            theme_switch: self.theme_switch,
            desktop_icons: self.desktop_icons,
            reader_default: self.reader_default,
            reader_zoom: self.zoom_default,
            card_attachments: self.card_attachments,
            attachment_drawer: self.drawer_enabled,
            thread_expansion: self.thread_expansion,
            thread_row_newest: self.thread_row_newest,
            confirm_thread_delete: self.confirm_thread_delete,
            message_theme: self.message_theme,
            override_fonts: self.override_fonts,
            reader_font: self.reader_font.clone(),
            override_colors: self.override_colors,
            plain_monospace: self.plain_monospace,
            plain_font: self.plain_font.clone(),
            compose_format: self.compose_format,
            follow_plain: self.follow_plain,
            reply_position: self.reply_position,
            signature_position: self.signature_position,
            signature_dashes: self.signature_dashes,
            reply_header: self.reply_header.clone(),
            undo_send_secs: self.undo_send_secs,
            notifications: self.notifications_enabled,
            notification_content: self.notification_content,
            notification_buttons: self.notification_buttons,
            show_attachments: self.show_attachments,
            show_unified: self.show_unified_pref,
            unified_chips: self.unified_chips,
            unified_kinds: self.unified_kinds,
            show_accounts: self.show_accounts,
            section_style: self.section_style,
            filtered_placement: self.filtered_placement,
            tags_placement: self.tags_placement,
            favorites_placement: self.favorites_placement,
            contacts_placement: self.contacts_placement,
            chevrons_left: self.chevrons_left,
            account_click_folds: self.account_click_folds,
            start_view: self.start_view,
            folder_sort: self.folder_sort,
            console_mode: self.console_mode,
            status_bar_top: self.status_bar_top,
            read_mark: self.read_mark,
            settings_open_accounts: self.settings_open_accounts,
            sidebar_hover_expand: self.sidebar_hover_expand,
            remember_sidebar: self.remember_sidebar,
            remember_rail: self.remember_rail,
            rail_dots: self.rail_dots,
            seen_counts: self.seen_counts,
            rail_fold: self.rail_fold,
            reader_toolbar: self.reader_toolbar.clone(),
            focus: self.focus,
            card_actions_hover: self.card_actions_hover,
            card_actions_auto: self.card_actions_auto,
            list_palette: self.list_palette,
            list_palette_hover: self.list_palette_hover,
            card_palette_menu: self.card_palette_menu,
            swipe_enabled: self.swipe_enabled,
            swipe_reversed: self.swipe_reversed,
            swipe_sensitivity: self.swipe_sensitivity,
            compose_inline: self.compose_inline,
            reply_inline: self.reply_inline,
            reply_fields: self.reply_fields,
            files: self.files_prefs,
            link_browser: self.link_browser.clone(),
            compose_default_from: self.compose_default_from.clone(),
            paste_plain: self.paste_plain,
            return_paragraph: self.return_paragraph,
            toolbar_expanded: self.toolbar_expanded,
            compose_font: self.compose_font.clone(),
            compose_font_size: self.compose_font_size,
            compose_zoom: self.compose_zoom,
            print_options: self.print_options,
            spellcheck: self.spellcheck,
            spellcheck_langs: self.spellcheck_langs.clone(),
            app_theme: self.app_theme,
            text_scale: self.text_scale,
            theme: self.theme.clone(),
            preview_lines: self.preview_lines,
            list_layout: self.list_layout,
            list_columns: self.list_columns.clone(),
            list_headings: self.list_headings,
            list_column_scroll: self.list_column_scroll,
            list_group_by_date: self.list_group_by_date,
            list_pills_below: self.list_pills_below,
            list_search_open: self.list_search_open,
            contacts_search_open: self.contacts_search_open,
            list_search_bottom: self.list_search_bottom,
            list_bulk_bar_bottom: self.list_bulk_bar_bottom,
            toolbar_fold_right: self.toolbar_fold_right,
            single_key_shortcuts: self.single_key.get(),
            gmail_keys: self.gmail_keys.get(),
            keep_downloads_days: self.keep_downloads_days,
            run_in_background: self.run_in_background.get(),
            autostart: self.autostart,
            start_fullscreen: self.start_fullscreen,
            tray: self.tray_enabled,
            tray_icon: self.tray_icon,
            tray_mail: self.tray_mail,
            launcher_badge: self.launcher_badge,
            app_icon: self.app_icon.clone(),
            accounts_panel: accounts.widget().clone().upcast::<gtk::Widget>(),
            accounts_sender: accounts.sender().clone(),
            // Sidebar order, so the list reads like the accounts do (#261).
            identities: self.identities_in_sidebar_order(),
            start_on_accounts: on_accounts,
            start_page: if on_accounts {
                Some("accounts".to_string())
            } else {
                self.last_settings_page.clone()
            },
        }
    }

    /// Every enabled account's identities, its own address and then its
    /// aliases, in sidebar order so the list reads like the accounts do
    /// (#261). An alias without a name of its own takes the account's.
    fn identities_in_sidebar_order(&self) -> Vec<(String, String)> {
        let mut enabled: Vec<&AccountConfig> = self.config.iter().filter(|a| a.enabled).collect();
        enabled.sort_by_key(|a| {
            self.account_order
                .iter()
                .position(|e| *e == a.email)
                .unwrap_or(usize::MAX)
        });
        enabled
            .into_iter()
            .flat_map(|a| {
                std::iter::once((a.name.clone(), a.email.clone())).chain(a.aliases.iter().map(
                    |al| {
                        let (name, addr) = crate::config::split_identity(&al.identity);
                        (
                            if name.is_empty() {
                                a.name.clone()
                            } else {
                                name
                            },
                            addr,
                        )
                    },
                ))
            })
            .collect()
    }

    /// Showcase hooks for the Settings window (demo only), each a beat
    /// after the panel is up.
    fn arm_settings_showcase(
        &self,
        accounts: &Controller<AccountsWindow>,
        sender: &ComponentSender<Self>,
    ) {
        // Showcase hook: HYLKI_SHOWCASE_EDIT_FILTER=<index> opens that
        // filter rule's editor once the panel is up, for a capture.
        // Sent through the app so it reaches whichever panel is current
        // when it fires: an open right after the pre-warm may have swapped
        // in a fresh one.
        if let Some(Ok(i)) = std::env::var("HYLKI_SHOWCASE_EDIT_FILTER")
            .ok()
            .map(|v| v.parse::<usize>())
        {
            if demo_mode() {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    s.input(AppMsg::ShowcaseEditFilter(i));
                });
            }
        }
        // HYLKI_SHOWCASE_FIND_TAGS=1 runs the tag finder once the panel is
        // up (the demo backend answers with a fixed set); the report dialog
        // captures itself two seconds after it appears.
        if std::env::var("HYLKI_SHOWCASE_FIND_TAGS").is_ok() && demo_mode() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(2, move || s.input(AppMsg::FindTags));
        }
        // HYLKI_SHOWCASE_EDIT_TAG=<index> likewise opens that tag's editor
        // (#147); an index past the end opens the Add Tag dialog.
        if let Some(Ok(i)) = std::env::var("HYLKI_SHOWCASE_EDIT_TAG")
            .ok()
            .map(|v| v.parse::<usize>())
        {
            if demo_mode() {
                let a = accounts.sender().clone();
                let n = self.tags.len();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    let _ = a.send(if i < n {
                        crate::ui::accounts::AccountsInput::EditTag(i)
                    } else {
                        crate::ui::accounts::AccountsInput::AddTag
                    });
                });
            }
        }
        // HYLKI_SHOWCASE_EDIT_SMART=<index> likewise opens that smart
        // folder's editor (#417); an index past the end opens a new one.
        if let Some(Ok(i)) = std::env::var("HYLKI_SHOWCASE_EDIT_SMART")
            .ok()
            .map(|v| v.parse::<usize>())
        {
            if demo_mode() {
                let a = accounts.sender().clone();
                let n = self.smart_folders.len();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    let _ = a.send(if i < n {
                        crate::ui::accounts::AccountsInput::EditSmartFolder(i)
                    } else {
                        crate::ui::accounts::AccountsInput::AddSmartFolder
                    });
                });
            }
        }
    }

    /// A fresh accounts panel over the current accounts (in display
    /// order), filters, tags and sender lists. Passwords stay in the keyring
    /// until an account's editor opens (the panel fetches them then, off
    /// the main thread): reading every account's secrets up front cost a
    /// keyring round trip or three per account, which is what made this
    /// window slow to appear with many accounts.
    fn accounts_panel_init(&self) -> crate::ui::accounts::AccountsInit {
        let order = self.ordered_emails();
        let mut accounts: Vec<AccountConfig> = Vec::new();
        for email in &order {
            if let Some(a) = self.config.iter().find(|c| &c.email == email) {
                accounts.push(a.clone());
            }
        }
        for c in &self.config {
            if !accounts.iter().any(|a| a.email == c.email) {
                accounts.push(c.clone());
            }
        }
        // Demo mode: the sample accounts exist only at the backend layer, so
        // the Accounts panel would sit empty in screenshots — hand it
        // matching stand-in configs instead.
        if accounts.is_empty() && demo_mode() {
            accounts = self.demo_config.clone();
        }
        // The accounts panel component (embedded behind the "Accounts" tab).
        crate::ui::accounts::AccountsInit {
            accounts,
            allowed_senders: self.allowed_senders.clone(),
            blacklist: self.blacklist.clone(),
            filters: self.filters.clone(),
            tags: self.tags.clone(),
            smart_folders: self.smart_folders.clone(),
        }
    }

    /// Launch an accounts panel over `init`.
    fn launch_accounts_panel(
        init: crate::ui::accounts::AccountsInit,
        sender: &ComponentSender<Self>,
    ) -> Controller<AccountsWindow> {
        let accounts = AccountsWindow::builder().launch(init).forward(
            sender.input_sender(),
            |out| match out {
                AccountsOutput::Saved {
                    original_email,
                    account,
                } => AppMsg::AccountSaved {
                    original_email,
                    account,
                },
                AccountsOutput::Removed { email } => AppMsg::AccountRemoved { email },
                AccountsOutput::Reordered(emails) => AppMsg::AccountsReordered(emails),
                AccountsOutput::EnabledChanged { email, enabled } => {
                    AppMsg::AccountEnabledChanged { email, enabled }
                }
                AccountsOutput::ImportGoa(account) => AppMsg::ImportGoaAccount(account),
                AccountsOutput::EditorOpen(open) => AppMsg::SettingsEditorOpen(open),
                AccountsOutput::AddSender(addr) => AppMsg::AddSender(addr),
                AccountsOutput::RemoveSender(addr) => AppMsg::RemoveSender(addr),
                AccountsOutput::AddBlacklist(addr) => AppMsg::AddBlacklist(addr),
                AccountsOutput::RemoveBlacklist(addr) => AppMsg::RemoveBlacklist(addr),
                AccountsOutput::SetFilters(rules) => AppMsg::SetFilters(rules),
                AccountsOutput::ApplyFilters => AppMsg::ApplyFilters(Vec::new()),
                AccountsOutput::SetTags(tags) => AppMsg::SetTags(tags),
                AccountsOutput::SetSmartFolders(folders) => AppMsg::SetSmartFolders(folders),
                AccountsOutput::FindTags => AppMsg::FindTags,
                AccountsOutput::WantQuota(email) => AppMsg::WantQuota(email),
                AccountsOutput::WantIdentities(email) => AppMsg::WantIdentities(email),
                AccountsOutput::LeftEditor(page) => {
                    AppMsg::SettingsLeaveEditor { page, ask: false }
                }
                AccountsOutput::LeaveNeedsPrompt(page) => {
                    AppMsg::SettingsLeaveEditor { page, ask: true }
                }
            },
        );

        // The host window: the preferences component, carrying the accounts
        // panel behind its other tab.
        accounts
    }

    /// Opens Settings on the accounts side at the page `page_id`, then
    /// hands its accounts panel `input` (an editor to open, a rule to add).
    pub(super) fn open_settings_at(
        &mut self,
        sender: &ComponentSender<Self>,
        page_id: &str,
        input: crate::ui::accounts::AccountsInput,
    ) {
        self.open_settings_window(sender, true, false);
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::ShowPageById(page_id.to_string()));
        }
        if let Some(acc) = &self.accounts_win {
            acc.emit(input);
        }
    }

    /// Show Settings: the window kept from a previous open (its accounts
    /// panel rebuilt over the current accounts) or a newly built one.
    ///
    /// Open (or focus) the combined Settings window on the
    /// requested panel. When `add_new`, jump straight to the "add account"
    /// form — used by the empty-state "Add first account" button.
    pub(super) fn open_settings_window(
        &mut self,
        sender: &ComponentSender<Self>,
        on_accounts: bool,
        add_new: bool,
    ) {
        let t_open = std::time::Instant::now();
        let hidden = self
            .prefs
            .as_ref()
            .is_some_and(|p| !p.widget().is_visible());
        if self.prefs.is_none() {
            self.build_settings_window(sender, on_accounts);
        } else if hidden {
            // Accounts, filters, tags and senders may have changed since: a
            // fresh panel over today's state — or the kept one, when
            // nothing did, with any editor it was left in closed.
            let init = self.accounts_panel_init();
            let seed = format!("{init:?}");
            let page = if on_accounts {
                "accounts".to_string()
            } else {
                self.last_settings_page
                    .clone()
                    .unwrap_or_else(|| "general".to_string())
            };
            if self.accounts_seed.as_deref() == Some(seed.as_str()) {
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::CloseEditor);
                }
            } else {
                let accounts = Self::launch_accounts_panel(init, sender);
                // A fresh panel knows no folders until told: without this
                // the filter editor's "Move to" list held only "Leave in
                // Inbox" whenever Settings reopened after a change.
                accounts.emit(crate::ui::accounts::AccountsInput::SetFolderChoices(
                    self.folder_choice_map(),
                ));
                self.accounts_seed = Some(seed);
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::SetAccountsPanel {
                        panel: accounts.widget().clone().upcast(),
                        sender: accounts.sender().clone(),
                    });
                }
                self.accounts_win = Some(accounts);
            }
            if let Some(p) = &self.prefs {
                p.emit(PrefInput::EditorOpen(None));
                p.emit(PrefInput::ShowPageById(page));
            }
        } else if on_accounts {
            // Already showing: switch to Accounts if asked, else leave the
            // window on whatever category it is showing.
            if let Some(p) = &self.prefs {
                p.emit(PrefInput::ShowAccounts(true));
            }
        }
        if let Some(p) = &self.prefs {
            p.widget().present();
        }
        if add_new {
            if let Some(a) = &self.accounts_win {
                a.emit(crate::ui::accounts::AccountsInput::AddAccount);
            }
        }
        tracing::debug!("settings window: presented in {:?}", t_open.elapsed());
        {
            let t = t_open;
            gtk::glib::idle_add_local_once(move || {
                tracing::debug!(
                    "settings window: first idle after open at {:?}",
                    t.elapsed()
                );
            });
            if let Some(p) = &self.prefs {
                p.widget().add_tick_callback(move |_, _| {
                    tracing::debug!("settings window: first frame at {:?}", t.elapsed());
                    gtk::glib::ControlFlow::Break
                });
            }
        }
    }

    /// Confirm and remove an account (drops its keyring password too).
    pub(super) fn confirm_remove_account(&self, account_id: u32, sender: &ComponentSender<Self>) {
        let Some(email) = self.email_of(account_id) else {
            return;
        };
        let label = self.account_label(account_id);
        let s = sender.clone();
        confirm_destructive(
            Some(self.window.upcast_ref()),
            &i18n("Remove Account?"),
            &format!(
                "Remove {label} from Hylki? Its saved password is deleted. \
                 Mail on the server is not affected."
            ),
            "remove",
            &i18n("Remove"),
            move || s.input(AppMsg::AccountRemoved { email }),
        );
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_settings_window(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ShowSettingsPage(id) => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ShowPageById(id));
                }
            }

            AppMsg::OpenSettings => {
                // The "opens to" preference decides the first open of the
                // session; after that the window returns to where it was.
                let on_accounts = self.settings_open_accounts && self.last_settings_page.is_none();
                self.open_settings_window(&sender, on_accounts, false);
            }

            AppMsg::SettingsPageShown(id) => {
                self.last_settings_page = Some(id);
            }

            AppMsg::OpenPreferences => self.open_settings_window(&sender, false, false),

            // Closing the combined Settings window hides it (the window's
            // own hide-on-close); both panels stay for the next open, which
            // then only rebuilds the accounts panel.
            AppMsg::ClosePreferences => {
                // The window only hides; let the signature editor's web
                // process go with it (#221).
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::ReleaseSignatureEditor);
                }
            }

            // Build the Settings window ahead of its first open, hidden and
            // realized, a moment after startup: its first appearance is
            // then a present() rather than a build, a layout and a paint.
            AppMsg::DebugCloseSettings => {
                if let Some(p) = &self.prefs {
                    p.widget().close();
                }
            }

            AppMsg::PrewarmSettings => {
                if self.prefs.is_none() {
                    self.build_settings_window(&sender, self.settings_open_accounts);
                    if let Some(p) = &self.prefs {
                        let w = p.widget();
                        WidgetExt::realize(w);
                        // Measuring runs the first CSS pass and Pango layouts
                        // now, so the first frame after present has less to do.
                        let _ = w.measure(gtk::Orientation::Horizontal, -1);
                        let _ = w.measure(gtk::Orientation::Vertical, 920);
                    }
                }
            }

            AppMsg::SettingsEditorOpen(open) => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::EditorOpen(open));
                }
            }

            AppMsg::SettingsLeaveEditor { page, ask } => {
                if let Some(p) = &self.prefs {
                    p.emit(if ask {
                        PrefInput::LeaveEditorPrompt(page)
                    } else {
                        PrefInput::LeaveEditorTo(page)
                    });
                }
            }

            AppMsg::ServerIdentities {
                account_id,
                identities,
            } => {
                if let (Some(email), Some(a)) = (self.email_of(account_id), &self.accounts_win) {
                    a.emit(crate::ui::accounts::AccountsInput::ServerIdentities {
                        email,
                        identities: identities.clone(),
                    });
                }
                self.server_identities.insert(account_id, identities);
            }
            AppMsg::WantIdentities(email) => {
                let Some(id) = self
                    .accounts
                    .iter()
                    .find(|a| a.email.eq_ignore_ascii_case(&email))
                    .map(|a| a.id)
                else {
                    return;
                };
                let identities = self.server_identities.get(&id).cloned().unwrap_or_default();
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::ServerIdentities {
                        email,
                        identities,
                    });
                }
            }
            _ => unreachable!("not routed to on_settings_window"),
        }
    }
}
