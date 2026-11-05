//! First-run welcome wizard: a guided, five-step setup shown when Hylki starts
//! with no accounts configured.
//!
//! The whole window is the theme's own ground, light or dark, with the wordmark as the
//! hero, and content floating on window-colored cards — deliberate and warm,
//! not a form dump. Steps: welcome → import from GNOME Online Accounts
//! (skippable) → add an account by hand (provider presets, custom OAuth) →
//! privacy → personalize → done. Accounts and settings apply through the app's existing
//! pipelines ([`WelcomeOutput`]), so the wizard owns no persistence of its own.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::Protocol;
use crate::config::{blank_account, AccountConfig};
use crate::i18n::{i18n, i18n_f};
use crate::ui::accounts::proton_verify::{self, HumanCheck};
use crate::ui::accounts::{
    hide_sign_in_rows, is_sign_in_step, mailbox_password, reveal_sign_in_row, totp_code, Provider,
    SignInRows, PROVIDERS,
};
use crate::worker::{self, ConnTest};

mod goa;
mod manual;
mod messages;
mod pages;
mod steps;
mod wordmark;

use goa::rebuild_goa_rows;
use manual::{form_has_input, wizard_providers};
pub(crate) use messages::{WelcomeCmd, WelcomeInput, WelcomeOutput, WelcomePrefs};
use pages::{
    build_done_page, build_goa_page, build_hero_page, build_personalize_page, build_privacy_page,
    build_wordmark, card, page, pill, scrolled, title, GoaPage, PersonalizePage, PrivacyPage,
    WordmarkParts,
};
use wordmark::{bind_wordmark_to_position, HERO_SIZE, HERO_TOP};
pub(crate) use wordmark::{
    wordmark_height, wordmark_height_of, wordmark_picture, wordmark_picture_of, Wordmark,
};

pub(crate) struct Welcome {
    goa: Vec<crate::services::goa::GoaMailAccount>,
    /// Emails added so far (either path), shown on the account page and used
    /// to relabel the final button.
    added: Vec<String>,
    /// Generation counter for connection tests: clicking Test & Add again
    /// supersedes the running test (its late result is ignored), so the
    /// button never has to lock while a slow/wrong server times out.
    test_seq: u32,
    /// Something was imported from Online Accounts, or added by hand: that
    /// page's footer button reads Continue rather than Skip.
    goa_imported: bool,
    manual_added: bool,
    /// Proton's human verification for the manual form's sign-in.
    human_check: HumanCheck,
}

pub(crate) struct WelcomeWidgets {
    carousel: adw::Carousel,
    back_btn: gtk::Button,
    wordmark_frame: gtk::Box,
    wordmark_box: gtk::Box,
    // Account page.
    provider_row: adw::ComboRow,
    hint_lbl: gtk::Label,
    name_row: adw::EntryRow,
    email_row: adw::EntryRow,
    pass_row: adw::PasswordEntryRow,
    /// Tuta's and Proton's second-factor code, shown once a sign-in asks
    /// for it.
    totp_row: adw::EntryRow,
    /// Proton's mailbox password, for accounts with two passwords; shown
    /// once a sign-in asks for it.
    mailbox_pass_row: adw::PasswordEntryRow,
    /// Proton's human verification, shown once a sign-in asks for it.
    verify_row: adw::ActionRow,
    server_exp: adw::ExpanderRow,
    host_row: adw::EntryRow,
    port_row: adw::EntryRow,
    smtp_row: adw::EntryRow,
    smtp_port_row: adw::EntryRow,
    oauth_client_id_row: adw::EntryRow,
    oauth_secret_row: adw::PasswordEntryRow,
    oauth_auth_url_row: adw::EntryRow,
    oauth_token_url_row: adw::EntryRow,
    oauth_scope_row: adw::EntryRow,
    status_lbl: gtk::Label,
    test_spinner: gtk::Spinner,
    add_btn: gtk::Button,
    /// The manual page's title, which becomes "Add another email account"
    /// once one came in from Online Accounts.
    acct_title: gtk::Label,
    // Online Accounts page.
    goa_empty: gtk::Label,
    goa_list: gtk::ListBox,
    goa_status: gtk::Label,
    footer_next: gtk::Button,
    // Prefs pages.
    sw_remote: adw::SwitchRow,
    sw_gravatar: adw::SwitchRow,
    sw_logos: adw::SwitchRow,
    sw_notif: adw::SwitchRow,
    preview_row: adw::ComboRow,
    sw_avatars: adw::SwitchRow,
    sw_threading: adw::SwitchRow,
    /// The gallery's latest pick, read when the wizard finishes.
    icon_choice: std::rc::Rc<std::cell::RefCell<String>>,
}

/// Extra space above the Online Accounts page's list (or its empty note)
/// and its buttons.
const GOA_GAP: i32 = 26;

impl Component for Welcome {
    type Init = ();
    type Input = WelcomeInput;
    type Output = WelcomeOutput;
    type CommandOutput = WelcomeCmd;
    type Root = adw::Window;
    type Widgets = WelcomeWidgets;

    fn init_root() -> Self::Root {
        let win = adw::Window::new();
        win.set_default_size(600, 700);
        win.add_css_class("welcome-window");
        win.set_title(Some(i18n("Welcome to Hylki").as_str()));
        win
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let existing: Vec<String> = crate::config::load()
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.email.to_ascii_lowercase())
            .collect();
        let mut goa = crate::services::goa::list_mail_accounts();
        goa.retain(|g| g.can_connect() && !existing.contains(&g.email.to_ascii_lowercase()));
        let model = Welcome {
            goa,
            added: Vec::new(),
            test_seq: 0,
            goa_imported: false,
            manual_added: false,
            human_check: HumanCheck::default(),
        };

        let carousel = adw::Carousel::new();
        carousel.set_interactive(false); // guided: buttons drive the flow
        carousel.set_hexpand(true);
        carousel.set_vexpand(true);

        let WordmarkParts {
            frame: wordmark_frame,
            overlay: wordmark_overlay,
            holder: wordmark_box,
        } = build_wordmark();
        build_hero_page(&carousel, &sender, &wordmark_overlay);
        let GoaPage {
            list: goa_list,
            empty: goa_empty,
            status: goa_status,
        } = build_goa_page(&carousel, &model, &sender);

        // ---- Page 3: an account by hand ----
        let acct = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let acct_title = title(&i18n("Add your email account"));
        acct.append(&acct_title);

        let form = card();
        let provider_row = adw::ComboRow::new();
        provider_row.set_title(&i18n("Provider"));
        let labels: Vec<&str> = wizard_providers().map(|p| p.wizard_label()).collect();
        provider_row.set_model(Some(&gtk::StringList::new(&labels)));
        // The providers' marks before their names, as in Settings.
        provider_row.set_factory(Some(&crate::ui::accounts::provider_factory()));
        // Default to the plain IMAP/POP3 entry.
        let manual = wizard_providers()
            .position(|p| p.wizard_is_manual())
            .unwrap_or(0);
        provider_row.set_selected(manual as u32);
        {
            let s = sender.clone();
            provider_row.connect_selected_notify(move |_| {
                s.input_sender().emit(WelcomeInput::ProviderChanged)
            });
        }
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Your name"));
        let email_row = adw::EntryRow::new();
        email_row.set_title(&i18n("Email address"));
        let pass_row = adw::PasswordEntryRow::new();
        pass_row.set_title(&i18n("Password"));
        // Hidden until Tuta or Proton asks for it (`show_sign_in_failure`).
        let totp_row = adw::EntryRow::new();
        totp_row.set_title(&i18n("Authenticator code"));
        totp_row.set_input_purpose(gtk::InputPurpose::Digits);
        totp_row.set_visible(false);
        // Likewise, until Proton asks for either.
        let mailbox_pass_row = adw::PasswordEntryRow::new();
        mailbox_pass_row.set_title(&i18n("Mailbox password"));
        mailbox_pass_row.set_visible(false);
        let verify_row = adw::ActionRow::new();
        verify_row.set_title(&i18n("Human verification"));
        verify_row.set_subtitle(&i18n("Proton wants to confirm a person is signing in."));
        verify_row.set_visible(false);
        {
            let verify_btn = gtk::Button::with_label(&i18n("Verify…"));
            verify_btn.set_valign(gtk::Align::Center);
            let s = sender.clone();
            verify_btn.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::VerifyHuman));
            verify_row.add_suffix(&verify_btn);
        }
        let server_exp = adw::ExpanderRow::new();
        server_exp.set_title(&i18n("Server details"));
        server_exp.set_subtitle(&i18n("Filled in for known providers"));
        // The default provider is the plain "IMAP/POP3 Account" entry,
        // whose whole point is filling these in — start them open. Picking a
        // known provider collapses them (ProviderChanged), picking the
        // generic one re-opens them.
        server_exp.set_expanded(true);
        let host_row = adw::EntryRow::new();
        host_row.set_title(&i18n("IMAP server"));
        let port_row = adw::EntryRow::new();
        port_row.set_title(&i18n("IMAP port"));
        port_row.set_text("993");
        let smtp_row = adw::EntryRow::new();
        smtp_row.set_title(&i18n("SMTP server"));
        let smtp_port_row = adw::EntryRow::new();
        smtp_port_row.set_title(&i18n("SMTP port"));
        smtp_port_row.set_text("587");
        server_exp.add_row(&host_row);
        server_exp.add_row(&port_row);
        server_exp.add_row(&smtp_row);
        server_exp.add_row(&smtp_port_row);
        // Custom OAuth's client, the same fields as the account editor's.
        let oauth_client_id_row = adw::EntryRow::new();
        oauth_client_id_row.set_title(&i18n("OAuth Client ID"));
        let oauth_secret_row = adw::PasswordEntryRow::new();
        oauth_secret_row.set_title(&i18n("OAuth Client Secret (optional)"));
        let oauth_auth_url_row = adw::EntryRow::new();
        oauth_auth_url_row.set_title(&i18n("Authorization URL"));
        let oauth_token_url_row = adw::EntryRow::new();
        oauth_token_url_row.set_title(&i18n("Token URL"));
        let oauth_scope_row = adw::EntryRow::new();
        oauth_scope_row.set_title(&i18n("Scopes (space-separated)"));
        form.append(&provider_row);
        form.append(&name_row);
        form.append(&email_row);
        form.append(&pass_row);
        form.append(&totp_row);
        form.append(&mailbox_pass_row);
        form.append(&verify_row);
        for row in [
            oauth_client_id_row.upcast_ref::<gtk::Widget>(),
            oauth_secret_row.upcast_ref(),
            oauth_auth_url_row.upcast_ref(),
            oauth_token_url_row.upcast_ref(),
            oauth_scope_row.upcast_ref(),
        ] {
            row.set_visible(false);
            form.append(row);
        }
        form.append(&server_exp);
        acct.append(&form);

        let hint_lbl = gtk::Label::new(None);
        hint_lbl.add_css_class("welcome-hint");
        hint_lbl.set_halign(gtk::Align::Start);
        hint_lbl.set_wrap(true);
        hint_lbl.set_visible(false);
        acct.append(&hint_lbl);

        let add_btn = pill(&i18n("Test & Add"));
        add_btn.set_margin_top(4);
        {
            let s = sender.clone();
            add_btn.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::TestAndAdd));
        }
        acct.append(&add_btn);
        // The result line sits under the button, account name and verdict
        // together on one line — with a spinner while a test runs.
        let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_box.set_halign(gtk::Align::Center);
        let test_spinner = gtk::Spinner::new();
        test_spinner.set_visible(false);
        let status_lbl = gtk::Label::new(None);
        status_lbl.add_css_class("welcome-hint");
        status_lbl.set_wrap(true);
        status_lbl.set_justify(gtk::Justification::Center);
        status_box.append(&test_spinner);
        status_box.append(&status_lbl);
        acct.append(&status_box);

        // Full width, as the form was when the Online Accounts note beside
        // it set the page's width.
        let acct_page = page(&acct);
        acct_page.set_halign(gtk::Align::Fill);
        carousel.append(&scrolled(&acct_page));

        let PrivacyPage {
            remote: sw_remote,
            gravatar: sw_gravatar,
            logos: sw_logos,
            notif: sw_notif,
        } = build_privacy_page(&carousel);
        let PersonalizePage {
            preview_row,
            avatars: sw_avatars,
            threading: sw_threading,
            icon_choice,
        } = build_personalize_page(&carousel);
        build_done_page(&carousel);

        // ---- Chrome: draggable top strip with Back button ----
        let tv = adw::ToolbarView::new();
        let hb = adw::HeaderBar::new();
        hb.set_show_title(false);
        hb.add_css_class("flat");
        let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
        back_btn.add_css_class("flat");
        back_btn.set_visible(false);
        {
            let s = sender.clone();
            back_btn.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::Back));
        }
        hb.pack_start(&back_btn);
        tv.add_top_bar(&hb);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&wordmark_box);
        content.append(&carousel);
        // Fixed footer: Continue lives with the dots so it never scrolls
        // away with a tall page, and reads Finish on the last one. Hidden on
        // the hero page, which has its own Get Started.
        let footer_next = pill(&i18n("Continue"));
        footer_next.set_visible(false);
        footer_next.set_margin_top(16);
        footer_next.set_margin_bottom(8);
        {
            let s = sender.clone();
            footer_next.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::Next));
        }
        content.append(&footer_next);
        let dots = adw::CarouselIndicatorDots::new();
        dots.set_carousel(Some(&carousel));
        dots.set_margin_bottom(14);
        content.append(&dots);
        {
            let btn = footer_next.clone();
            let s = sender.clone();
            carousel.connect_position_notify(move |car| {
                let r = car.position().round() as u32;
                btn.set_visible(r >= 1);
                s.input_sender().emit(WelcomeInput::RefreshFooter);
            });
        }
        // Typing in the manual form turns its Skip into Continue.
        for entry in [
            name_row.upcast_ref::<gtk::Editable>(),
            totp_row.upcast_ref(),
            mailbox_pass_row.upcast_ref(),
            email_row.upcast_ref(),
            pass_row.upcast_ref(),
            host_row.upcast_ref(),
            smtp_row.upcast_ref(),
            oauth_client_id_row.upcast_ref(),
            oauth_secret_row.upcast_ref(),
            oauth_auth_url_row.upcast_ref(),
            oauth_token_url_row.upcast_ref(),
            oauth_scope_row.upcast_ref(),
        ] {
            let s = sender.clone();
            entry.connect_changed(move |_| s.input_sender().emit(WelcomeInput::RefreshFooter));
        }
        tv.set_content(Some(&content));
        root.set_content(Some(&tv));

        let widgets = WelcomeWidgets {
            carousel,
            back_btn,
            wordmark_frame,
            wordmark_box,
            provider_row,
            hint_lbl,
            name_row,
            email_row,
            pass_row,
            totp_row,
            mailbox_pass_row,
            verify_row,
            server_exp,
            host_row,
            port_row,
            smtp_row,
            smtp_port_row,
            oauth_client_id_row,
            oauth_secret_row,
            oauth_auth_url_row,
            oauth_token_url_row,
            oauth_scope_row,
            status_lbl,
            test_spinner,
            add_btn,
            goa_empty,
            goa_list,
            goa_status,
            footer_next,
            acct_title,
            sw_remote,
            sw_gravatar,
            sw_logos,
            sw_notif,
            preview_row,
            sw_avatars,
            sw_threading,
            icon_choice,
        };
        rebuild_goa_rows(&widgets.goa_list, &model.goa, &sender);
        bind_wordmark_to_position(
            &widgets.carousel,
            &widgets.wordmark_frame,
            &widgets.wordmark_box,
        );

        // Screenshot hook, mirroring the main window's: HYLKI_SHOWCASE=path
        // captures the wizard after a beat; HYLKI_SHOWCASE_PAGE=N walks to
        // that page first.
        if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
            let win = root.clone();
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(4, move || {
                // Walk pages via Next so the capture includes everything a
                // real click drives (the wordmark choreography included).
                let n = std::env::var("HYLKI_SHOWCASE_PAGE")
                    .unwrap_or_default()
                    .parse::<u32>()
                    .unwrap_or(0);
                // Spaced out so each Next lands after the previous scroll
                // settles (position() reads fractional mid-scroll).
                for i in 0..n {
                    let s = s.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(700 * i as u64),
                        move || s.input_sender().emit(WelcomeInput::Next),
                    );
                }
                let win = win.clone();
                let path = path.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(2000 + 700 * n as u64),
                    move || {
                        crate::ui::showcase::showcase_capture(win.upcast_ref(), &path);
                    },
                );
            });
        }
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            WelcomeInput::LanguageChanged(i) => self.on_language_changed(i, sender),
            WelcomeInput::Next => self.on_next(widgets, sender),
            WelcomeInput::Back => self.on_back(widgets),
            WelcomeInput::ProviderChanged => self.on_provider_changed(widgets),
            WelcomeInput::TestAndAdd => self.on_test_and_add(widgets, sender),
            WelcomeInput::VerifyHuman => self.on_verify_human(widgets, &sender),
            WelcomeInput::HumanVerified(verified) => {
                if self.human_check.solve(verified) {
                    self.on_test_and_add(widgets, sender);
                }
            }
            WelcomeInput::ImportGoa(index) => self.on_import_goa(index, widgets, sender),
            WelcomeInput::RescanGoa => self.on_rescan_goa(widgets, sender),
            WelcomeInput::OpenOnlineAccounts => crate::ui::accounts::open_online_accounts(),
            WelcomeInput::RefreshFooter => self.refresh_footer(widgets),
            WelcomeInput::Finish => self.on_finish(widgets, sender, root),
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            WelcomeCmd::Tested {
                account,
                result,
                seq,
            } => self.on_tested(account, result, seq, widgets, sender),
            WelcomeCmd::HelperSignedIn {
                account,
                result,
                seq,
            } => self.on_helper_signed_in(account, result, seq, widgets, sender),
            WelcomeCmd::SignedIn {
                account,
                result,
                seq,
            } => self.on_signed_in(account, result, seq, widgets, sender),
        }
    }
}

impl Welcome {
    /// The footer button reads Skip on the two account pages while nothing
    /// was added there and, on the manual page, nothing is typed in the
    /// form; Finish on the last page; Continue everywhere else.
    fn refresh_footer(&self, widgets: &WelcomeWidgets) {
        let page = widgets.carousel.position().round() as u32;
        let label = if page + 1 >= widgets.carousel.n_pages() {
            i18n("Finish")
        } else if match page {
            1 => !self.goa_imported,
            2 => !self.manual_added && !form_has_input(widgets),
            _ => false,
        } {
            i18n("Skip")
        } else {
            i18n("Continue")
        };
        widgets.footer_next.set_label(&label);
    }
}
