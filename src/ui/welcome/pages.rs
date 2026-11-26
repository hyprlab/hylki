//! The pieces every wizard page is made of: the scrolling page, its title
//! and tagline, cards, pills, and the entrance animation.

use super::*;

/// Wrap a page so a tall one scrolls instead of forcing the window taller
/// (the carousel's minimum height is its tallest page's).
pub(super) fn scrolled(inner: &gtk::Box) -> gtk::ScrolledWindow {
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
pub(super) fn page(content: &gtk::Box) -> gtk::Box {
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

pub(super) fn title(text: &str) -> gtk::Label {
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

pub(super) fn card() -> gtk::ListBox {
    let lb = gtk::ListBox::new();
    lb.add_css_class("boxed-list");
    lb.add_css_class("welcome-card");
    lb.set_selection_mode(gtk::SelectionMode::None);
    lb
}

pub(super) fn pill(label: &str) -> gtk::Button {
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

/// The wordmark's parts: the frame the animation sizes, the overlay
/// holding the picture, and the box that places it.
pub(super) struct WordmarkParts {
    pub(super) frame: gtk::Box,
    pub(super) overlay: gtk::Overlay,
    pub(super) holder: gtk::Box,
}

/// The wordmark: lives ABOVE the carousel, on every page. It
/// starts hero-sized and mid-window; Get Started shrinks it to 100px
/// and floats it to the top (see WelcomeInput::Next), where it stays.
/// The positioning margins live on the box and the size on the Clamp,
/// so the entrance animation (which drives the clamp's own margin)
/// never fights the choreography.
/// Sizing a Picture directly is a trap twice over: width_request is a
/// floor, and the texture's natural size wins over the request under
/// centering. The reliable cap (see the palette's clip saga) is an
/// Overlay whose MAIN child is an empty spacer — an empty box's
/// natural size is exactly its size request, so the animation drives
/// the spacer and the clipped Picture simply fills whatever the
/// overlay was given.
pub(super) fn build_wordmark() -> WordmarkParts {
    let wordmark_pic = wordmark_picture_of(Wordmark::WithIconWhite, 300);
    let wordmark_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    wordmark_frame.set_size_request(
        HERO_SIZE as i32,
        wordmark_height_of(Wordmark::WithIconWhite, HERO_SIZE),
    );
    let wordmark_overlay = gtk::Overlay::new();
    wordmark_overlay.set_child(Some(&wordmark_frame));
    wordmark_overlay.add_overlay(&wordmark_pic);
    wordmark_overlay.set_clip_overlay(&wordmark_pic, true);
    wordmark_overlay.set_halign(gtk::Align::Center);
    let wordmark_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    wordmark_box.set_halign(gtk::Align::Center);
    wordmark_box.set_margin_top(HERO_TOP);
    wordmark_box.append(&wordmark_overlay);
    WordmarkParts {
        frame: wordmark_frame,
        overlay: wordmark_overlay,
        holder: wordmark_box,
    }
}

/// Page 1: hero (tagline + start; the wordmark floats above).
pub(super) fn build_hero_page(
    carousel: &adw::Carousel,
    sender: &ComponentSender<Welcome>,
    wordmark_overlay: &gtk::Overlay,
) {
    let hero = gtk::Box::new(gtk::Orientation::Vertical, 18);
    hero.set_halign(gtk::Align::Center);
    let tag = tagline(&i18n(
        "A clean, fast home for your mail.\nSetting up takes about a minute.",
    ));
    tag.set_margin_top(16);
    // The interface language, first of all (#179): System, English and
    // every shipped translation. A pick restarts Hylki into it, so the
    // rest of the wizard reads in the chosen language.
    let lang_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    lang_row.set_halign(gtk::Align::Center);
    // 64px below the tagline: the hero's 18px spacing plus this.
    lang_row.set_margin_top(46);
    let lang_label = gtk::Label::new(Some(i18n("Language").as_str()));
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
        lang.connect_selected_notify(move |d| {
            s.input_sender()
                .emit(WelcomeInput::LanguageChanged(d.selected()))
        });
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
        start.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::Next));
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
    if let Some(n) = std::env::var("HYLKI_SHOWCASE_WIZARD_LANG")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        let lang = lang.clone();
        gtk::glib::timeout_add_seconds_local_once(4, move || lang.set_selected(n));
    }
}

/// The Online Accounts page's list, its empty note and its status line.
pub(super) struct GoaPage {
    pub(super) list: gtk::ListBox,
    pub(super) empty: gtk::Label,
    pub(super) status: gtk::Label,
}

/// Page 2: GNOME Online Accounts.
///
/// The accounts already signed in on this computer, Google among
/// them (its only sign-in), one click each. Microsoft has its own
/// sign-in on the next page (#329). Nothing here is required: the
/// footer reads Skip until something is added.
pub(super) fn build_goa_page(
    carousel: &adw::Carousel,
    model: &Welcome,
    sender: &ComponentSender<Welcome>,
) -> GoaPage {
    let goa_pg = gtk::Box::new(gtk::Orientation::Vertical, 14);
    let import = gtk::Image::from_icon_name("arrow-into-box-symbolic");
    import.set_pixel_size(36);
    import.add_css_class("welcome-check");
    goa_pg.append(&import);
    goa_pg.append(&title(&i18n("Import your online accounts")));
    goa_pg.append(&tagline(&i18n(
        "Mail accounts signed in under GNOME Settings → Online Accounts can be added here in one click. Google accounts are only supported through GNOME Online Accounts.",
    )));
    let goa_list = card();
    goa_list.set_visible(!model.goa.is_empty());
    // As far from the tagline as the buttons are from the list.
    goa_list.set_margin_top(GOA_GAP);
    goa_pg.append(&goa_list);
    let goa_empty = tagline(&i18n("No mail accounts were found in Online Accounts."));
    goa_empty.add_css_class("welcome-hint");
    goa_empty.set_visible(model.goa.is_empty());
    // The empty note sits as far from the tagline as the buttons sit
    // from it: 47px each to the eye, page spacing and margin together.
    goa_empty.set_margin_top(GOA_GAP);
    goa_pg.append(&goa_empty);
    // Hidden until an import reports, so an empty line never widens the
    // gap above the buttons.
    let goa_status = gtk::Label::new(None);
    goa_status.add_css_class("welcome-hint");
    goa_status.set_wrap(true);
    goa_status.set_justify(gtk::Justification::Center);
    goa_status.set_visible(false);
    goa_pg.append(&goa_status);
    let goa_links = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    goa_links.set_halign(gtk::Align::Center);
    // 4px more: a button's edge has none of a text line's leading.
    goa_links.set_margin_top(GOA_GAP + 4);
    let open_goa = gtk::Button::with_label(&i18n("Open Online Accounts…"));
    let rescan = gtk::Button::with_label(&i18n("Scan Again"));
    for b in [&open_goa, &rescan] {
        b.add_css_class("pill");
        b.add_css_class("welcome-secondary");
    }
    {
        let s = sender.clone();
        open_goa.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::OpenOnlineAccounts));
        let s = sender.clone();
        rescan.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::RescanGoa));
    }
    goa_links.append(&open_goa);
    goa_links.append(&rescan);
    goa_pg.append(&goa_links);
    carousel.append(&scrolled(&page(&goa_pg)));
    GoaPage {
        list: goa_list,
        empty: goa_empty,
        status: goa_status,
    }
}

/// The privacy page's switches.
pub(super) struct PrivacyPage {
    pub(super) remote: adw::SwitchRow,
    pub(super) gravatar: adw::SwitchRow,
    pub(super) logos: adw::SwitchRow,
    pub(super) notif: adw::SwitchRow,
}

/// Page 4: privacy.
pub(super) fn build_privacy_page(carousel: &adw::Carousel) -> PrivacyPage {
    let priv_pg = gtk::Box::new(gtk::Orientation::Vertical, 14);
    priv_pg.append(&title(&i18n("Privacy, your way")));
    priv_pg.append(&tagline(&i18n(
        "Hylki sends no telemetry. These control what leaves your computer while you read.",
    )));
    let priv_card = card();
    let sw_remote = adw::SwitchRow::new();
    sw_remote.set_title(&i18n("Block remote images"));
    sw_remote.set_subtitle(&i18n("Stops senders tracking when and where you open mail"));
    sw_remote.set_active(!crate::config::load_privacy().auto_remote_content);
    let sw_gravatar = adw::SwitchRow::new();
    sw_gravatar.set_title(&i18n("Use Gravatar when a contact has no photo"));
    sw_gravatar.set_subtitle(&i18n("Sends a hash of the sender’s address to Gravatar."));
    sw_gravatar.set_active(crate::config::load_privacy().gravatar);
    let sw_logos = adw::SwitchRow::new();
    sw_logos.set_title(&i18n("Show sender logos"));
    sw_logos.set_subtitle(&i18n("Looks up company icons for the message list"));
    sw_logos.set_active(crate::config::load_privacy().sender_logos);
    let sw_notif = adw::SwitchRow::new();
    sw_notif.set_title(&i18n("Show message content in notifications"));
    sw_notif.set_subtitle(&i18n("Off keeps senders and subjects off the lock screen"));
    sw_notif.set_active(crate::config::load_privacy().notification_content);
    priv_card.append(&sw_remote);
    priv_card.append(&sw_gravatar);
    priv_card.append(&sw_logos);
    priv_card.append(&sw_notif);
    priv_pg.append(&priv_card);
    carousel.append(&scrolled(&page(&priv_pg)));
    PrivacyPage {
        remote: sw_remote,
        gravatar: sw_gravatar,
        logos: sw_logos,
        notif: sw_notif,
    }
}

/// The personalize page's rows, and the app icon picked in its strip.
pub(super) struct PersonalizePage {
    pub(super) preview_row: adw::ComboRow,
    pub(super) avatars: adw::SwitchRow,
    pub(super) threading: adw::SwitchRow,
    pub(super) icon_choice: std::rc::Rc<std::cell::RefCell<String>>,
}

/// Page 5: personalize.
pub(super) fn build_personalize_page(carousel: &adw::Carousel) -> PersonalizePage {
    let pers = gtk::Box::new(gtk::Orientation::Vertical, 14);
    pers.append(&title(&i18n("Make it yours")));
    pers.append(&tagline(&i18n(
        "A few popular choices. Everything can be changed later in Settings.",
    )));
    let pers_card = card();
    let preview_row = adw::ComboRow::new();
    preview_row.set_title(&i18n("Preview lines"));
    preview_row.set_subtitle(&i18n("Message text shown under each subject"));
    preview_row.set_model(Some(&gtk::StringList::new(&[
        i18n("None").as_str(),
        i18n("1 line").as_str(),
        i18n("2 lines").as_str(),
    ])));
    preview_row.set_selected(crate::config::load_preview_lines().min(2));
    let sw_avatars = adw::SwitchRow::new();
    sw_avatars.set_title(&i18n("Show sender avatars"));
    sw_avatars.set_subtitle(&i18n("Colorful initials beside each message"));
    sw_avatars.set_active(crate::config::load_privacy().avatars);
    let sw_threading = adw::SwitchRow::new();
    sw_threading.set_title(&i18n("Group messages by conversation"));
    sw_threading.set_active(crate::config::load_privacy().threading);
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
    // With an icon set outside Hylki on the launcher (#252), nothing is
    // preselected, and only an actual pick replaces it.
    let icon_choice = std::rc::Rc::new(std::cell::RefCell::new(
        if crate::desktop::app_icon::custom_icon().is_some() {
            String::new()
        } else {
            crate::config::load_app_icon()
                .unwrap_or_else(|| crate::desktop::app_icon::DEFAULT_ID.to_string())
        },
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
    PersonalizePage {
        preview_row,
        avatars: sw_avatars,
        threading: sw_threading,
        icon_choice,
    }
}

/// Page 6: done.
pub(super) fn build_done_page(carousel: &adw::Carousel) {
    let done = gtk::Box::new(gtk::Orientation::Vertical, 16);
    let check = gtk::Image::from_icon_name("verified-checkmark-symbolic");
    check.set_pixel_size(72);
    check.add_css_class("welcome-check");
    done.append(&check);
    done.append(&title(&i18n("You are all set")));
    let done_sub = tagline("");
    done_sub.set_use_markup(true);
    done_sub.set_markup(&i18n_f(
        "To report a problem or ask for a feature,\nopen an issue on <a href=\"{url}\">GitHub</a>.",
        &[("url", "https://github.com/hyprlab/hylki")],
    ));
    done.append(&done_sub);
    carousel.append(&scrolled(&page(&done)));
}
