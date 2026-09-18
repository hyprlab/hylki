//! First-run welcome wizard: a guided, five-step setup shown when Hylki starts
//! with no accounts configured.
//!
//! The whole window wears the icon's yellow (#fec200) with the wordmark as the
//! hero, and content floating on window-coloured cards — deliberate and warm,
//! not a form dump. Steps: welcome → add an account (one-click GNOME Online
//! Accounts imports + a manual IMAP form with provider presets) → privacy →
//! personalize → done. Accounts and settings apply through the app's existing
//! pipelines ([`WelcomeOutput`]), so the wizard owns no persistence of its own.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::AccountConfig;
use crate::ui::accounts::{Provider, PROVIDERS};
use crate::worker::{self, ConnTest};
use crate::i18n::{i18n, i18n_f};

/// Wordmark art, embedded so the wizard needs nothing on disk.
const WORDMARK_SVG: &[u8] = include_bytes!("../../data/welcome/wordmark-black.svg");

/// The settings chosen on the privacy + personalize pages, applied by the app
/// through its normal Set* handlers when the wizard finishes.
#[derive(Debug, Clone)]
pub struct WelcomePrefs {
    pub block_remote: bool,
    pub gravatar: bool,
    pub sender_logos: bool,
    pub notification_content: bool,
    pub preview_lines: u32,
    pub avatars: bool,
    pub threading: bool,
    /// The app icon picked from the gallery (an `app_icon::catalog` id).
    pub app_icon: String,
}

#[derive(Debug)]
pub enum WelcomeInput {
    Next,
    Back,
    ProviderChanged,
    TestAndAdd,
    ImportGoa(usize),
    RescanGoa,
    Finish,
    /// The hero page's language drop-down moved to choice `n`.
    LanguageChanged(u32),
}

#[derive(Debug)]
pub enum WelcomeOutput {
    /// A manual account passed its connection test: save + connect it.
    AddAccount(Box<AccountConfig>),
    /// A GNOME Online Accounts import was chosen.
    ImportGoa(Box<AccountConfig>),
    /// The chosen privacy/personalize settings (sent once, on finish).
    Prefs(WelcomePrefs),
    /// The wizard is done (window already closing).
    Done,
    /// A language was picked on the first page: its locale code, "" for
    /// the system's. The app saves it and comes back in that language.
    Language(String),
}

#[derive(Debug)]
pub enum WelcomeCmd {
    Tested { account: Box<AccountConfig>, result: ConnTest, seq: u32 },
}

pub struct Welcome {
    goa: Vec<crate::goa::GoaMailAccount>,
    /// Emails added so far (either path), shown on the account page and used
    /// to relabel the final button.
    added: Vec<String>,
    /// Generation counter for connection tests: clicking Test & Add again
    /// supersedes the running test (its late result is ignored), so the
    /// button never has to lock while a slow/wrong server times out.
    test_seq: u32,
}

pub struct WelcomeWidgets {
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
    server_exp: adw::ExpanderRow,
    host_row: adw::EntryRow,
    port_row: adw::EntryRow,
    smtp_row: adw::EntryRow,
    smtp_port_row: adw::EntryRow,
    status_lbl: gtk::Label,
    test_spinner: gtk::Spinner,
    add_btn: gtk::Button,
    goa_card: gtk::Box,
    goa_list: gtk::ListBox,
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
    finish_btn: gtk::Button,
}

/// The password-capable subset of the shared provider table (OAuth providers
/// go through GNOME Online Accounts, which the page covers separately).
fn wizard_providers() -> Vec<&'static Provider> {
    PROVIDERS.iter().filter(|p| p.wizard_password_provider()).collect()
}

/// Render the wordmark at 2x for crisp HiDPI, displayed at `width` px.
/// (Also used by the About window's identity block.)
pub(crate) fn wordmark_picture(width: i32) -> gtk::Picture {
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    // Render the SVG at the requested size (2x for crisp HiDPI).
    loader.connect_size_prepared(move |l, w, h| {
        let scale = (width * 2) as f64 / w.max(1) as f64;
        l.set_size(width * 2, (h as f64 * scale) as i32);
    });
    let pic = gtk::Picture::new();
    if loader.write(WORDMARK_SVG).is_ok() && loader.close().is_ok() {
        if let Some(pb) = loader.pixbuf() {
            pic.set_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&pb)));
        }
    }
    pic.set_can_shrink(true);
    pic.set_content_fit(gtk::ContentFit::Contain);
    pic
}

/// Wrap a page so a tall one scrolls instead of forcing the window taller
/// (the carousel's minimum height is its tallest page's).
fn scrolled(inner: &gtk::Box) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    // ScrolledWindow deliberately doesn't propagate its child's expand flags
    // (it is the scrollable boundary), so without these the carousel sizes
    // the page to its natural width and neighbours peek in at the edges.
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    sw.set_child(Some(inner));
    sw
}

/// One wizard page: vertically centered content in consistent margins.
fn page(content: &gtk::Box) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_valign(gtk::Align::Center);
    outer.set_halign(gtk::Align::Center);
    outer.set_margin_top(24);
    outer.set_margin_bottom(24);
    outer.set_margin_start(48);
    outer.set_margin_end(48);
    outer.set_hexpand(true);
    outer.set_vexpand(true);
    outer.append(content);
    outer
}

fn title(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("welcome-title");
    l.set_halign(gtk::Align::Center);
    l
}

fn tagline(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("welcome-tagline");
    l.set_halign(gtk::Align::Center);
    l.set_justify(gtk::Justification::Center);
    l.set_wrap(true);
    l
}

fn card() -> gtk::ListBox {
    let lb = gtk::ListBox::new();
    lb.add_css_class("boxed-list");
    lb.add_css_class("welcome-card");
    lb.set_selection_mode(gtk::SelectionMode::None);
    lb
}

fn pill(label: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("pill");
    b.add_css_class("suggested-action");
    b.add_css_class("welcome-pill");
    b.set_halign(gtk::Align::Center);
    b
}

/// Fade-and-rise entrance for the hero page, staggered per widget. Runs on the
/// window's first map (animations on unmapped widgets skip to the end).
fn entrance(widgets: &[gtk::Widget]) {
    for (i, w) in widgets.iter().cloned().enumerate() {
        w.set_opacity(0.0);
        // The rise is relative: each widget ends back on its OWN margin, so
        // layout spacing set at build time survives the animation.
        let base = w.margin_top();
        gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(120 + 140 * i as u64),
            move || {
                let target = adw::CallbackAnimationTarget::new({
                    let w = w.clone();
                    move |v| {
                        w.set_opacity(v);
                        w.set_margin_top(base + ((1.0 - v) * 24.0) as i32);
                    }
                });
                let anim = adw::TimedAnimation::new(&w, 0.0, 1.0, 600, target);
                anim.set_easing(adw::Easing::EaseOutCubic);
                anim.play();
            },
        );
    }
}

/// Hero and shrunk geometry for the floating wordmark.
const HERO_TOP: i32 = 150;
const HERO_SIZE: f64 = 240.0;
const SMALL_TOP: f64 = 6.0;
const SMALL_BOTTOM: f64 = 16.0;
const SMALL_SIZE: f64 = 100.0;

/// The wordmark art's aspect (its viewBox is 433x125).
const WORDMARK_ASPECT: f64 = 125.0 / 433.0;

/// The wordmark's height for a given width.
pub(crate) fn wordmark_height(width: f64) -> i32 {
    (width * WORDMARK_ASPECT) as i32
}

/// Tie the wordmark's size and lift to the carousel's live position: while
/// the spring runs from the hero to page 2, the wordmark shrinks and rises in
/// lockstep (a shared-element move — no second timeline to fall out of sync),
/// and it parks small for every later page.
fn bind_wordmark_to_position(carousel: &adw::Carousel, frame: &gtk::Box, holder: &gtk::Box) {
    let frame = frame.clone();
    let holder = holder.clone();
    carousel.connect_position_notify(move |car| {
        let p = car.position().clamp(0.0, 1.0);
        let s = HERO_SIZE + (SMALL_SIZE - HERO_SIZE) * p;
        frame.set_size_request(s as i32, wordmark_height(s));
        holder
            .set_margin_top((HERO_TOP as f64 + (SMALL_TOP - HERO_TOP as f64) * p) as i32);
        // The small form also keeps 16px of air below itself; the hero's
        // spacing is the tagline's own margin, so this fades in with p.
        holder.set_margin_bottom((SMALL_BOTTOM * p) as i32);
    });
}

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
        let mut goa = crate::goa::list_mail_accounts();
        goa.retain(|g| !existing.contains(&g.email.to_ascii_lowercase()));
        let model = Welcome {
            goa,
            added: Vec::new(),
            test_seq: 0,
        };

        let carousel = adw::Carousel::new();
        carousel.set_interactive(false); // guided: buttons drive the flow
        carousel.set_hexpand(true);
        carousel.set_vexpand(true);

        // ---- The wordmark: lives ABOVE the carousel, on every page. It
        // starts hero-sized and mid-window; Get Started shrinks it to 100px
        // and floats it to the top (see WelcomeInput::Next), where it stays.
        // The positioning margins live on the box and the size on the Clamp,
        // so the entrance animation (which drives the clamp's own margin)
        // never fights the choreography.
        // Sizing a Picture directly is a trap twice over: width_request is a
        // floor, and the texture's natural size wins over the request under
        // centering. The reliable cap (see the palette's clip saga) is an
        // Overlay whose MAIN child is an empty spacer — an empty box's
        // natural size is exactly its size request, so the animation drives
        // the spacer and the clipped Picture simply fills whatever the
        // overlay was given.
        let wordmark_pic = wordmark_picture(300);
        let wordmark_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wordmark_frame.set_size_request(HERO_SIZE as i32, wordmark_height(HERO_SIZE));
        let wordmark_overlay = gtk::Overlay::new();
        wordmark_overlay.set_child(Some(&wordmark_frame));
        wordmark_overlay.add_overlay(&wordmark_pic);
        wordmark_overlay.set_clip_overlay(&wordmark_pic, true);
        wordmark_overlay.set_halign(gtk::Align::Center);
        let wordmark_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wordmark_box.set_halign(gtk::Align::Center);
        wordmark_box.set_margin_top(HERO_TOP);
        wordmark_box.append(&wordmark_overlay);

        // ---- Page 1: hero (tagline + start; the wordmark floats above) ----
        let hero = gtk::Box::new(gtk::Orientation::Vertical, 18);
        hero.set_halign(gtk::Align::Center);
        let tag = tagline(&i18n("A clean, fast home for your mail.\nLet's set things up — it takes about a minute."));
        tag.set_margin_top(16);
        // The interface language, first of all (#179): System, English and
        // every shipped translation. A pick restarts Hylki into it, so the
        // rest of the wizard reads in the chosen language.
        let lang_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        lang_row.set_halign(gtk::Align::Center);
        // 64px below the tagline: the hero's 18px spacing plus this.
        lang_row.set_margin_top(46);
        let lang_label = gtk::Label::new(Some(i18n("Language").as_str()));
        // On the yellow, text is always dark (the theme's dim grey turns
        // white in dark mode).
        lang_label.add_css_class("welcome-hint");
        lang_row.append(&lang_label);
        let choices = crate::ui::preferences::language_choices();
        let choice_labels: Vec<&str> = choices.iter().map(|(l, _)| l.as_str()).collect();
        let lang = gtk::DropDown::from_strings(&choice_labels);
        lang.set_valign(gtk::Align::Center);
        let current = crate::config::load_language();
        lang.set_selected(choices.iter().position(|(_, c)| *c == current).unwrap_or(0) as u32);
        {
            let s = sender.clone();
            lang.connect_selected_notify(move |d| s.input(WelcomeInput::LanguageChanged(d.selected())));
        }
        lang_row.append(&lang);
        // Fair warning: a pick restarts the app.
        let lang_note = gtk::Label::new(Some(
            i18n("Choosing another language restarts Hylki: it closes for a moment and this wizard comes back in that language.").as_str(),
        ));
        lang_note.add_css_class("welcome-hint");
        lang_note.add_css_class("caption");
        lang_note.set_wrap(true);
        lang_note.set_justify(gtk::Justification::Center);
        lang_note.set_max_width_chars(46);
        lang_note.set_margin_top(2);
        let start = pill(&i18n("Get Started"));
        start.set_margin_top(10);
        {
            let s = sender.clone();
            start.connect_clicked(move |_| s.input(WelcomeInput::Next));
        }
        hero.append(&tag);
        hero.append(&lang_row);
        hero.append(&lang_note);
        hero.append(&start);
        let hero_page = page(&hero);
        hero_page.set_valign(gtk::Align::Start);
        hero_page.set_margin_top(6);
        carousel.append(&scrolled(&hero_page));
        entrance(&[
            wordmark_overlay.clone().upcast(),
            tag.clone().upcast(),
            lang_row.clone().upcast(),
            lang_note.clone().upcast(),
            start.clone().upcast(),
        ]);
        // HYLKI_SHOWCASE_WIZARD_LANG=<n> picks choice n on the drop-down
        // after 4 s, what a click there does (for testing the restart).
        if let Some(n) = std::env::var("HYLKI_SHOWCASE_WIZARD_LANG").ok().and_then(|v| v.parse::<u32>().ok()) {
            let lang = lang.clone();
            gtk::glib::timeout_add_seconds_local_once(4, move || lang.set_selected(n));
        }

        // ---- Page 2: account ----
        let acct = gtk::Box::new(gtk::Orientation::Vertical, 14);
        acct.append(&title(&i18n("Add your email account")));

        // One-click imports from GNOME Online Accounts.
        let goa_card_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let goa_hdr = gtk::Label::new(Some(i18n("Found in GNOME Online Accounts").as_str()));
        goa_hdr.add_css_class("welcome-section");
        goa_hdr.set_halign(gtk::Align::Start);
        let goa_list = card();
        goa_card_box.append(&goa_hdr);
        goa_card_box.append(&goa_list);
        goa_card_box.set_visible(!model.goa.is_empty());
        acct.append(&goa_card_box);

        // Manual form.
        let form = card();
        let provider_row = adw::ComboRow::new();
        provider_row.set_title(&i18n("Provider"));
        let labels: Vec<&str> = wizard_providers().iter().map(|p| p.wizard_label()).collect();
        provider_row.set_model(Some(&gtk::StringList::new(&labels)));
        // The providers' marks before their names, as in Settings.
        provider_row.set_factory(Some(&crate::ui::accounts::provider_factory()));
        // Default to the plain IMAP/POP3 entry.
        let manual = wizard_providers().iter().position(|p| p.wizard_is_manual()).unwrap_or(0);
        provider_row.set_selected(manual as u32);
        {
            let s = sender.clone();
            provider_row.connect_selected_notify(move |_| s.input(WelcomeInput::ProviderChanged));
        }
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Your name"));
        let email_row = adw::EntryRow::new();
        email_row.set_title(&i18n("Email address"));
        let pass_row = adw::PasswordEntryRow::new();
        pass_row.set_title(&i18n("Password"));
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
        form.append(&provider_row);
        form.append(&name_row);
        form.append(&email_row);
        form.append(&pass_row);
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
            add_btn.connect_clicked(move |_| s.input(WelcomeInput::TestAndAdd));
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

        let goa_note = tagline(&i18n("Google and Microsoft accounts sign in through GNOME Settings → Online Accounts, then appear above."));
        goa_note.add_css_class("welcome-hint");
        let rescan = gtk::Button::with_label(&i18n("Scan Again"));
        rescan.add_css_class("flat");
        rescan.add_css_class("welcome-link");
        rescan.set_halign(gtk::Align::Center);
        {
            let s = sender.clone();
            rescan.connect_clicked(move |_| s.input(WelcomeInput::RescanGoa));
        }
        let note_row = gtk::Box::new(gtk::Orientation::Vertical, 2);
        note_row.set_halign(gtk::Align::Center);
        note_row.append(&goa_note);
        note_row.append(&rescan);
        acct.append(&note_row);

        carousel.append(&scrolled(&page(&acct)));

        // ---- Page 3: privacy ----
        let priv_pg = gtk::Box::new(gtk::Orientation::Vertical, 14);
        priv_pg.append(&title(&i18n("Privacy, your way")));
        priv_pg.append(&tagline(&i18n("Hylki sends no telemetry, ever. These control what leaves your machine while you read.")));
        let priv_card = card();
        let sw_remote = adw::SwitchRow::new();
        sw_remote.set_title(&i18n("Block remote images"));
        sw_remote.set_subtitle(&i18n("Stops senders tracking when and where you open mail"));
        sw_remote.set_active(!crate::config::load_auto_remote_content());
        let sw_gravatar = adw::SwitchRow::new();
        sw_gravatar.set_title(&i18n("Fetch Gravatar portraits"));
        sw_gravatar.set_subtitle(&i18n("Asks gravatar.com about each sender's address"));
        sw_gravatar.set_active(crate::config::load_gravatar());
        let sw_logos = adw::SwitchRow::new();
        sw_logos.set_title(&i18n("Fetch sender logos"));
        sw_logos.set_subtitle(&i18n("Looks up company icons for the message list"));
        sw_logos.set_active(crate::config::load_sender_logos());
        let sw_notif = adw::SwitchRow::new();
        sw_notif.set_title(&i18n("Show message content in notifications"));
        sw_notif.set_subtitle(&i18n("Off keeps senders and subjects off the lock screen"));
        sw_notif.set_active(crate::config::load_notification_content());
        priv_card.append(&sw_remote);
        priv_card.append(&sw_gravatar);
        priv_card.append(&sw_logos);
        priv_card.append(&sw_notif);
        priv_pg.append(&priv_card);
        carousel.append(&scrolled(&page(&priv_pg)));

        // ---- Page 4: personalize ----
        let pers = gtk::Box::new(gtk::Orientation::Vertical, 14);
        pers.append(&title(&i18n("Make it yours")));
        pers.append(&tagline(&i18n("A few popular choices — everything can be changed later in Settings.")));
        let pers_card = card();
        let preview_row = adw::ComboRow::new();
        preview_row.set_title(&i18n("Preview lines"));
        preview_row.set_subtitle(&i18n("Message text shown under each subject"));
        preview_row.set_model(Some(&gtk::StringList::new(&[i18n("None").as_str(), i18n("1 line").as_str(), i18n("2 lines").as_str()])));
        preview_row.set_selected(crate::config::load_preview_lines().min(2));
        let sw_avatars = adw::SwitchRow::new();
        sw_avatars.set_title(&i18n("Sender avatars"));
        sw_avatars.set_subtitle(&i18n("Colourful initials beside each message"));
        sw_avatars.set_active(crate::config::load_avatars());
        let sw_threading = adw::SwitchRow::new();
        sw_threading.set_title(&i18n("Conversation view"));
        sw_threading.set_subtitle(&i18n("Group messages into threads"));
        sw_threading.set_active(crate::config::load_threading());
        pers_card.append(&preview_row);
        pers_card.append(&sw_avatars);
        pers_card.append(&sw_threading);
        pers.append(&pers_card);

        // The app icon: the whole set as one sideways-scrolling row on its
        // own card, the default ringed.
        let icon_hdr = gtk::Label::new(Some(i18n("Pick your app icon").as_str()));
        icon_hdr.add_css_class("welcome-section");
        icon_hdr.set_halign(gtk::Align::Start);
        icon_hdr.set_margin_top(6);
        pers.append(&icon_hdr);
        let icon_choice = std::rc::Rc::new(std::cell::RefCell::new(
            crate::config::load_app_icon().unwrap_or_else(|| crate::app_icon::DEFAULT_ID.to_string()),
        ));
        let icon_card = card();
        let icon_row = gtk::ListBoxRow::new();
        icon_row.set_activatable(false);
        icon_row.set_focusable(false);
        let strip = {
            let choice = icon_choice.clone();
            crate::ui::icon_picker::strip(
                &icon_choice.borrow(),
                52,
                std::rc::Rc::new(move |id: &str| *choice.borrow_mut() = id.to_string()),
            )
        };
        strip.set_margin_top(8);
        strip.set_margin_start(6);
        strip.set_margin_end(6);
        icon_row.set_child(Some(&strip));
        icon_card.append(&icon_row);
        pers.append(&icon_card);
        carousel.append(&scrolled(&page(&pers)));

        // ---- Page 5: done ----
        let done = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let check = gtk::Image::from_icon_name("co.hyprlab.Hylki-verified-checkmark-symbolic");
        check.set_pixel_size(72);
        check.add_css_class("welcome-check");
        done.append(&check);
        done.append(&title(&i18n("You're all set")));
        let done_sub = tagline("");
        done_sub.set_use_markup(true);
        done_sub.set_markup(&i18n_f(
            "Enjoy! If you encounter an issue or have a feature request,\nplease open an issue on our <a href=\"{url}\">Github</a>.",
            &[("url", "https://github.com/hyprlab/hylki")],
        ));
        done.append(&done_sub);
        let finish_btn = pill(&i18n("Start Reading"));
        finish_btn.set_margin_top(8);
        {
            let s = sender.clone();
            finish_btn.connect_clicked(move |_| s.input(WelcomeInput::Finish));
        }
        done.append(&finish_btn);
        carousel.append(&scrolled(&page(&done)));

        // ---- Chrome: draggable top strip with Back button ----
        let tv = adw::ToolbarView::new();
        let hb = adw::HeaderBar::new();
        hb.set_show_title(false);
        hb.add_css_class("flat");
        let back_btn = gtk::Button::from_icon_name("co.hyprlab.Hylki-go-previous-symbolic");
        back_btn.add_css_class("flat");
        back_btn.set_visible(false);
        {
            let s = sender.clone();
            back_btn.connect_clicked(move |_| s.input(WelcomeInput::Back));
        }
        hb.pack_start(&back_btn);
        tv.add_top_bar(&hb);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&wordmark_box);
        content.append(&carousel);
        // Fixed footer: Continue lives with the dots so it never scrolls
        // away with a tall page. Hidden on the hero and final pages, which
        // carry their own calls to action.
        let footer_next = pill(&i18n("Continue"));
        footer_next.set_visible(false);
        footer_next.set_margin_top(16);
        footer_next.set_margin_bottom(8);
        {
            let s = sender.clone();
            footer_next.connect_clicked(move |_| s.input(WelcomeInput::Next));
        }
        content.append(&footer_next);
        let dots = adw::CarouselIndicatorDots::new();
        dots.set_carousel(Some(&carousel));
        dots.set_margin_bottom(14);
        content.append(&dots);
        {
            let btn = footer_next.clone();
            carousel.connect_position_notify(move |car| {
                let r = car.position().round() as u32;
                btn.set_visible(r >= 1 && r + 1 < car.n_pages());
            });
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
            server_exp,
            host_row,
            port_row,
            smtp_row,
            smtp_port_row,
            status_lbl,
            test_spinner,
            add_btn,
            goa_card: goa_card_box,
            goa_list,
            sw_remote,
            sw_gravatar,
            sw_logos,
            sw_notif,
            preview_row,
            sw_avatars,
            sw_threading,
            icon_choice,
            finish_btn,
        };
        rebuild_goa_rows(&widgets.goa_list, &model.goa, &sender);
        bind_wordmark_to_position(&widgets.carousel, &widgets.wordmark_frame, &widgets.wordmark_box);

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
                        move || s.input(WelcomeInput::Next),
                    );
                }
                let win = win.clone();
                let path = path.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(2000 + 700 * n as u64),
                    move || {
                        crate::app::showcase_capture(win.upcast_ref(), &path);
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
            WelcomeInput::LanguageChanged(i) => {
                if let Some((_, code)) = crate::ui::preferences::language_choices().get(i as usize) {
                    let _ = sender.output(WelcomeOutput::Language(code.clone()));
                }
            }
            WelcomeInput::Next => {
                // position() is fractional mid-scroll; round to the page the
                // user sees as current, so rapid clicks don't repeat a page.
                let pos = widgets.carousel.position().round() as u32;
                if pos + 1 < widgets.carousel.n_pages() {
                    let next = widgets.carousel.nth_page(pos + 1);
                    widgets.carousel.scroll_to(&next, true);
                }
                widgets.back_btn.set_visible(true);
            }
            WelcomeInput::Back => {
                let pos = widgets.carousel.position().round() as usize;
                if pos > 0 {
                    let prev = widgets.carousel.nth_page(pos as u32 - 1);
                    widgets.carousel.scroll_to(&prev, true);
                    widgets.back_btn.set_visible(pos > 1);
                }
            }
            WelcomeInput::ProviderChanged => {
                let sel = widgets.provider_row.selected() as usize;
                if let Some(p) = wizard_providers().get(sel) {
                    let (ih, ip, sh, sp) = p.wizard_servers();
                    if !ih.is_empty() {
                        widgets.host_row.set_text(ih);
                        widgets.port_row.set_text(&ip.to_string());
                        widgets.smtp_row.set_text(sh);
                        widgets.smtp_port_row.set_text(&sp.to_string());
                        widgets.server_exp.set_expanded(false);
                    } else {
                        widgets.host_row.set_text("");
                        widgets.smtp_row.set_text("");
                        widgets.server_exp.set_expanded(true);
                    }
                    let hint = p.wizard_hint();
                    widgets.hint_lbl.set_visible(!hint.is_empty());
                    widgets.hint_lbl.set_text(hint);
                }
            }
            WelcomeInput::TestAndAdd => {
                let email = widgets.email_row.text().trim().to_string();
                let password = widgets.pass_row.text().to_string();
                // Derive missing servers from the address's domain — the common
                // convention, and the expander is right there to correct it.
                let domain = email.split('@').nth(1).unwrap_or("").to_string();
                let mut host = widgets.host_row.text().trim().to_string();
                if host.is_empty() && !domain.is_empty() {
                    host = format!("imap.{domain}");
                    widgets.host_row.set_text(&host);
                }
                let mut smtp = widgets.smtp_row.text().trim().to_string();
                if smtp.is_empty() && !domain.is_empty() {
                    smtp = format!("smtp.{domain}");
                    widgets.smtp_row.set_text(&smtp);
                }
                if email.is_empty() || password.is_empty() || host.is_empty() {
                    widgets.status_lbl.set_css_classes(&["welcome-hint", "error"]);
                    widgets
                        .status_lbl
                        .set_text(&i18n("Enter your email address and password first."));
                    return;
                }
                let account = AccountConfig {
                    name: widgets.name_row.text().trim().to_string(),
                    email: email.clone(),
                    imap_host: host,
                    imap_port: widgets.port_row.text().trim().parse().unwrap_or(993),
                    smtp_host: smtp,
                    smtp_port: widgets.smtp_port_row.text().trim().parse().unwrap_or(587),
                    username: email,
                    password,
                    ..blank_account()
                };
                self.test_seq += 1;
                let seq = self.test_seq;
                widgets.test_spinner.set_visible(true);
                widgets.test_spinner.start();
                widgets.status_lbl.set_css_classes(&["welcome-hint"]);
                widgets.status_lbl.set_text(&i18n("Checking the connection…"));
                sender.oneshot_command(async move {
                    let test = {
                        let account = account.clone();
                        tokio::task::spawn_blocking(move || worker::test_connection_blocking(account))
                            .await
                            .unwrap_or_else(|_| ConnTest {
                                incoming: Err("test could not run".into()),
                                smtp: Err("test could not run".into()),
                            })
                    };
                    WelcomeCmd::Tested { account: Box::new(account), result: test, seq }
                });
            }
            WelcomeInput::ImportGoa(index) => {
                if let Some(g) = self.goa.get(index).cloned() {
                    let (password, oauth) = if g.password_based {
                        (crate::goa::mail_passwords(&g.id).0.unwrap_or_default(), false)
                    } else {
                        (String::new(), true)
                    };
                    let account = g.to_config(password, oauth);
                    self.added.push(account.email.clone());
                    self.goa.remove(index);
                    widgets.status_lbl.set_css_classes(&["welcome-hint", "success"]);
                    widgets
                        .status_lbl
                        .set_text(&i18n_f("✓ {email} — added", &[("email", &(account.email).to_string())]));
                    let _ = sender.output(WelcomeOutput::ImportGoa(Box::new(account)));
                    rebuild_goa_rows(&widgets.goa_list, &self.goa, &sender);
                    widgets.goa_card.set_visible(!self.goa.is_empty());
                    self.show_added(widgets);
                }
            }
            WelcomeInput::RescanGoa => {
                self.goa = crate::goa::list_mail_accounts();
                self.goa.retain(|g| !self.added.contains(&g.email));
                rebuild_goa_rows(&widgets.goa_list, &self.goa, &sender);
                widgets.goa_card.set_visible(!self.goa.is_empty());
            }
            WelcomeInput::Finish => {
                let prefs = WelcomePrefs {
                    block_remote: widgets.sw_remote.is_active(),
                    gravatar: widgets.sw_gravatar.is_active(),
                    sender_logos: widgets.sw_logos.is_active(),
                    notification_content: widgets.sw_notif.is_active(),
                    preview_lines: widgets.preview_row.selected(),
                    avatars: widgets.sw_avatars.is_active(),
                    threading: widgets.sw_threading.is_active(),
                    app_icon: widgets.icon_choice.borrow().clone(),
                };
                let _ = sender.output(WelcomeOutput::Prefs(prefs));
                let _ = sender.output(WelcomeOutput::Done);
                root.close();
            }
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
            WelcomeCmd::Tested { account, result, seq } => {
                if seq != self.test_seq {
                    // A newer test was started (the user corrected a field and
                    // clicked again); this stale verdict no longer matters.
                    return;
                }
                widgets.test_spinner.stop();
                widgets.test_spinner.set_visible(false);
                let email = account.email.clone();
                match (&result.incoming, &result.smtp) {
                    (Ok(_), Ok(_)) => {
                        self.added.push(account.email.clone());
                        widgets.status_lbl.set_css_classes(&["welcome-hint", "success"]);
                        widgets.status_lbl.set_text(&i18n_f("✓ {email} — connected and added", &[("email", &(email).to_string())]));
                        widgets.pass_row.set_text("");
                        widgets.email_row.set_text("");
                        let _ = sender.output(WelcomeOutput::AddAccount(account));
                        self.show_added(widgets);
                    }
                    (Err(e), _) => {
                        widgets.status_lbl.set_css_classes(&["welcome-hint", "error"]);
                        widgets.status_lbl.set_text(&i18n_f("{email} — mail server: {e}", &[("email", &(email).to_string()), ("e", &(e).to_string())]));
                    }
                    (_, Err(e)) => {
                        widgets.status_lbl.set_css_classes(&["welcome-hint", "error"]);
                        widgets.status_lbl.set_text(&i18n_f("{email} — sending (SMTP): {e}", &[("email", &(email).to_string()), ("e", &(e).to_string())]));
                    }
                }
            }
        }
    }
}

impl Welcome {
    fn show_added(&self, widgets: &WelcomeWidgets) {
        widgets.finish_btn.set_label(&i18n("Start Reading"));
    }
}

fn rebuild_goa_rows(
    list: &gtk::ListBox,
    goa: &[crate::goa::GoaMailAccount],
    sender: &ComponentSender<Welcome>,
) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }
    for (i, g) in goa.iter().enumerate() {
        let row = adw::ActionRow::new();
        row.set_title(&g.email);
        row.set_subtitle(&g.provider);
        row.add_prefix(&crate::brand::image_or(
            crate::ui::accounts::brand_for_goa(&g.provider),
            24,
            crate::brand::GENERIC_MAIL,
        ));
        let btn = gtk::Button::with_label(&i18n("Add"));
        btn.add_css_class("suggested-action");
        btn.set_valign(gtk::Align::Center);
        let s = sender.clone();
        btn.connect_clicked(move |_| s.input(WelcomeInput::ImportGoa(i)));
        row.add_suffix(&btn);
        list.append(&row);
    }
}

/// A fully-defaulted account for the wizard's manual form to build on.
fn blank_account() -> AccountConfig {
    AccountConfig {
        name: String::new(),
        email: String::new(),
        protocol: Default::default(),
        imap_host: String::new(),
        imap_port: 993,
        smtp_host: String::new(),
        smtp_port: 587,
        username: String::new(),
        password: String::new(),
        smtp_separate: false,
        smtp_username: String::new(),
        smtp_password: String::new(),
        color: None,
        emoji: None,
        avatar: None,
        gravatar: false,
        signature: None,
        signature_html: false,
        label: None,
        aliases: Vec::new(),
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
        push: None,
        folder_roles: Default::default(),
        sent_copy_path: None,
        server_saves_sent: false,
        empty_junk_days: 0,
        empty_trash_days: 0,
        pgp_key: None,
    }
}
