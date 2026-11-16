//! The About window and the people it thanks.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n;

use super::model::AppModel;
use super::msg::AppMsg;
use super::release_notes::release_notes_page;

/// The contributors and translators shown in the About window's thanks lists.
/// Both come from the repository's metafiles, read in at build time: one
/// person per line, `Display Name <github-handle>`, with a translator's
/// languages after a dash. What each person contributed is credited in
/// `docs/CREDITS.md` and the changelog, not in the files.
const CONTRIBUTORS: &str = include_str!("../../data/CONTRIBUTORS");
const TRANSLATORS: &str = include_str!("../../data/TRANSLATORS");

/// One of those files as (display name, GitHub handle, note) rows. Comments,
/// blank lines and any line without a handle are skipped, so a typo in the
/// file costs that one row rather than the list.
fn credits(file: &'static str) -> Vec<(&'static str, &'static str, &'static str)> {
    file.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, rest) = line.split_once('<')?;
            let (handle, note) = rest.split_once('>')?;
            Some((
                name.trim(),
                handle.trim(),
                note.trim().trim_start_matches('-').trim(),
            ))
        })
        .collect()
}

/// A heading over one of the About page's lists.
fn section_heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("heading");
    label.set_halign(gtk::Align::Start);
    label.set_margin_top(20);
    label.set_margin_bottom(6);
    label
}

/// The wordmark, the version chip, the one-line description and, in a
/// beta build, the warning that it is one.
fn about_identity(page: &gtk::Box) {
    // Identity block: the wordmark with the app icon beside it,
    // wizard-style. Same Overlay-with-spacer cap as the wizard: a
    // Picture's texture wins over both width requests and clamps.
    use crate::ui::welcome::Wordmark;
    const ABOUT_WORDMARK: i32 = 180;
    let wm_pic = crate::ui::welcome::wordmark_picture_of(Wordmark::WithIcon, ABOUT_WORDMARK);
    let wm_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    wm_frame.set_size_request(
        ABOUT_WORDMARK,
        crate::ui::welcome::wordmark_height_of(Wordmark::WithIcon, f64::from(ABOUT_WORDMARK)),
    );
    let wm = gtk::Overlay::new();
    wm.set_child(Some(&wm_frame));
    wm.add_overlay(&wm_pic);
    wm.set_clip_overlay(&wm_pic, true);
    wm.set_halign(gtk::Align::Center);
    wm.set_margin_bottom(16);
    page.append(&wm);

    let version = gtk::Label::new(Some(crate::VERSION));
    version.add_css_class("about-version-chip");
    version.set_halign(gtk::Align::Center);
    version.set_margin_top(8);
    page.append(&version);

    // One-sentence description, directly under the version chip.
    let desc = gtk::Label::new(Some(i18n("A fast, GNOME-native email client").as_str()));
    desc.set_wrap(true);
    desc.set_justify(gtk::Justification::Center);
    desc.add_css_class("dim-label");
    desc.set_margin_top(12);
    page.append(&desc);

    if cfg!(feature = "beta") {
        let warn = gtk::Label::new(Some(
            i18n(
                "This is a beta build for trying upcoming changes early. \
             Expect bugs and instability — please report anything broken \
             on GitHub. It shares your accounts and mail with the stable \
             Hylki install.",
            )
            .as_str(),
        ));
        warn.set_wrap(true);
        warn.set_justify(gtk::Justification::Center);
        warn.add_css_class("warning");
        warn.set_margin_top(10);
        warn.set_margin_start(12);
        warn.set_margin_end(12);
        page.append(&warn);
    }
}

/// Release Notes (a sub-page of the window) and, on Linux Mint, the
/// keyring setup tip again.
fn about_info_rows(page: &gtk::Box, nav: &adw::NavigationView, sender: &ComponentSender<AppModel>) {
    // Release notes: slide in as a sub-page of this window.
    let info = gtk::ListBox::new();
    info.add_css_class("boxed-list");
    info.set_selection_mode(gtk::SelectionMode::None);
    info.set_margin_top(20);

    let notes_row = adw::ActionRow::builder()
        .title(i18n("Release Notes"))
        .subtitle(format!("What's new in {}", crate::VERSION))
        .activatable(true)
        .build();
    notes_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let nav = nav.clone();
        notes_row.connect_activated(move |_| nav.push_by_tag("notes"));
    }
    info.append(&notes_row);

    // The thanks lists outgrew the main page, so they slide in like the notes.
    let credits_row = adw::ActionRow::builder()
        .title(i18n("Credits"))
        .subtitle(i18n("Contributors and translators"))
        .activatable(true)
        .build();
    credits_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let nav = nav.clone();
        credits_row.connect_activated(move |_| nav.push_by_tag("credits"));
    }
    info.append(&credits_row);

    // Linux Mint (Cinnamon): re-open the one-time keyring setup tip. Shown only
    // where that tip applies, so Mint users who dismissed it can find it again.
    if crate::desktop::platform::is_mint_cinnamon() {
        let keyring_row = adw::ActionRow::builder()
            .title(i18n("Keyring Setup Help"))
            .subtitle(i18n("Make account passwords persist on Linux Mint"))
            .activatable(true)
            .build();
        keyring_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let sender = sender.clone();
        keyring_row.connect_activated(move |_| {
            sender.input(AppMsg::ShowKeyringHelp { problem: false });
        });
        info.append(&keyring_row);
    }

    page.append(&info);
}

/// The project's links.
fn about_links(page: &gtk::Box) {
    // Project links. Each row shows its URL as a hover tooltip.
    page.append(&section_heading(&i18n("Project")));

    let links = gtk::ListBox::new();
    links.add_css_class("boxed-list");
    links.set_selection_mode(gtk::SelectionMode::None);
    let mk_row = |title: &str, url: &str| -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .build();
        row.set_tooltip_text(Some(url));
        row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        let u = url.to_string();
        row.connect_activated(move |_| crate::services::oauth::open_uri(&u));
        row
    };
    links.append(&mk_row(&i18n("Website"), "https://hylki.hyprlab.co"));
    links.append(&mk_row(
        &i18n("Report an issue or feature request"),
        "https://github.com/hyprlab/hylki/issues",
    ));
    links.append(&mk_row("Discord", "https://discord.gg/YfEJ4b6PFW"));
    links.append(&mk_row("hyprlab@proton.me", "mailto:hyprlab@proton.me"));
    links.append(&mk_row(
        &i18n("Source Code"),
        "https://github.com/hyprlab/hylki",
    ));
    links.append(&mk_row(
        &i18n("License (GNU AGPL v3)"),
        "https://www.gnu.org/licenses/agpl-3.0.html",
    ));

    // Buy Me a Coffee — with a coffee-cup glyph as its leading icon.
    let coffee = adw::ActionRow::builder()
        .title(i18n("Buy Me a Coffee"))
        .activatable(true)
        .build();
    coffee.set_tooltip_text(Some("https://buymeacoffee.com/hyprlab"));
    let cup = gtk::Label::new(Some("☕"));
    cup.add_css_class("about-coffee");
    coffee.add_prefix(&cup);
    coffee.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    coffee.connect_activated(move |_| {
        crate::services::oauth::open_uri("https://buymeacoffee.com/hyprlab")
    });
    links.append(&coffee);
    page.append(&links);
}

/// One boxed list of people, each row opening their GitHub profile.
fn people_list(rows: Vec<(&'static str, &'static str, &'static str)>) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    for (name, handle, note) in rows {
        let subtitle = if note.is_empty() {
            format!("@{handle}")
        } else {
            format!("{note} · @{handle}")
        };
        let row = adw::ActionRow::builder()
            .title(name)
            .subtitle(subtitle)
            .activatable(true)
            .build();
        let url = format!("https://github.com/{handle}");
        row.set_tooltip_text(Some(&url));
        row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        row.connect_activated(move |_| crate::services::oauth::open_uri(&url));
        list.append(&row);
    }
    list
}

/// The Credits sub-page: the contributors and the translators.
fn credits_page() -> adw::NavigationPage {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_margin_bottom(24);

    // Contributors: people outside Hyprlab whose patches are in the app.
    let heading = section_heading(&i18n("Special thanks to these contributors"));
    heading.set_margin_top(6);
    page.append(&heading);
    page.append(&people_list(credits(CONTRIBUTORS)));

    // Translators: the same list, with the languages each one carried.
    page.append(&section_heading(&i18n("Translated by")));
    page.append(&people_list(credits(TRANSLATORS)));

    let clamp = adw::Clamp::builder().maximum_size(420).build();
    clamp.set_child(Some(&page));
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&scroller));

    adw::NavigationPage::builder()
        .title(i18n("Credits").as_str())
        .tag("credits")
        .child(&tv)
        .build()
}

/// The copyright line at the foot of the main page.
fn about_footer(page: &gtk::Box) {
    let footer = gtk::Label::new(Some(i18n("© 2026 Hyprlab").as_str()));
    footer.add_css_class("dim-label");
    footer.add_css_class("caption");
    footer.set_wrap(true);
    footer.set_justify(gtk::Justification::Center);
    footer.set_margin_top(20);
    page.append(&footer);
}

impl AppModel {
    /// A custom, scrollable About window: app identity up top (icon, name, a
    /// version chip, and a one-line description under it), then the feature
    /// sections laid out on the page itself, and project links.
    fn open_about(&self, sender: &ComponentSender<Self>) {
        let win = adw::Window::builder()
            .transient_for(&self.window)
            .modal(false)
            .title(format!("{} {}", i18n("About"), crate::APP_NAME).as_str())
            .default_width(460)
            // Remembered vertical size (tall by default) — resizing sticks
            // across restarts via the save on close below.
            .default_height(config::load_about_height())
            .build();
        win.connect_close_request(|w| {
            config::save_about_height(w.height());
            gtk::glib::Propagation::Proceed
        });

        // A navigation stack so Release Notes / Changelog slide in (and back out)
        // within the same window instead of spawning separate ones.
        let nav = adw::NavigationView::new();

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(420).build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.set_margin_top(18);
        page.set_margin_bottom(12);

        about_identity(&page);
        about_info_rows(&page, &nav, sender);
        about_links(&page);
        about_footer(&page);

        clamp.set_child(Some(&page));
        scroller.set_child(Some(&clamp));

        // The root page holds the identity + links; the sub-pages slide over it.
        let main_tv = adw::ToolbarView::new();
        let main_header = adw::HeaderBar::new();
        main_header.add_css_class("flat");
        main_header.set_show_title(false);
        main_tv.add_top_bar(&main_header);
        main_tv.set_content(Some(&scroller));
        nav.add(
            &adw::NavigationPage::builder()
                .title(format!("{} {}", i18n("About"), crate::APP_NAME).as_str())
                .tag("main")
                .child(&main_tv)
                .build(),
        );
        nav.add(&release_notes_page());
        nav.add(&credits_page());

        win.set_content(Some(&nav));
        win.present();

        // Screenshot hook: HYLKI_SHOWCASE_SCROLL runs the page down by that
        // fraction once it has been laid out, so a capture can show the
        // links at the bottom (the same variable does this for Settings).
        if let Some(frac) = std::env::var("HYLKI_SHOWCASE_SCROLL")
            .ok()
            .map(|v| v.parse::<f64>().unwrap_or(1.0))
        {
            let vadj = scroller.vadjustment();
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                vadj.set_value(vadj.lower() + (vadj.upper() - vadj.page_size()) * frac);
            });
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_about(&self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::OpenAbout => {
                self.open_about(&sender);
            }
            _ => unreachable!("not routed to on_about"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The credits metafiles are read at build time and shown in About, so a
    /// line that stops parsing loses a real person from the list silently.
    #[test]
    fn credits_files_parse() {
        for file in [CONTRIBUTORS, TRANSLATORS] {
            let rows = credits(file);
            let lines = file
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .count();
            assert_eq!(rows.len(), lines, "every non-comment line must parse");
            for (name, handle, _) in rows {
                assert!(!name.is_empty() && !handle.is_empty(), "{name}/{handle}");
                assert!(
                    !handle.contains(char::is_whitespace),
                    "{handle} is not a GitHub handle"
                );
            }
        }
    }

    /// Translators carry their languages after the dash; contributors carry
    /// nothing after the handle.
    #[test]
    fn credits_notes_are_translator_languages() {
        assert!(credits(CONTRIBUTORS)
            .iter()
            .all(|(_, _, note)| note.is_empty()));
        assert!(credits(TRANSLATORS)
            .iter()
            .all(|(_, _, note)| !note.is_empty()));
    }
}
