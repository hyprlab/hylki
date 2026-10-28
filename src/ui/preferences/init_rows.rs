//! Filling the settings rows from the saved values when the window is
//! built, one step per page, in the order `init` runs them.

use super::choices::{list_layout_index, section_style_index, start_view_index};
use super::rows::no_truncate;
use super::*;

impl Preferences {
    /// Settings never truncates: every combo gets a factory whose labels never
    /// ellipsize (see `no_truncate`).
    pub(super) fn make_combos_whole(&self, widgets: &PreferencesWidgets) {
        for row in [
            &widgets.general.fetch_row,
            &widgets.list.preview_lines_row,
            &widgets.reading.message_theme_row,
            &widgets.reading.card_actions_row,
            &widgets.appearance.app_theme_row,
            &widgets.appearance.text_scale_row,
            &widgets.appearance.settings_open_row,
            &widgets.system.tray_icon_row,
            &widgets.system.launcher_badge_row,
            &widgets.datetime.date_style_row,
            &widgets.datetime.clock_style_row,
            &widgets.system.language_row,
            &widgets.system.renderer_row,
            &widgets.sidebar.chevron_side_row,
            &widgets.sidebar.section_style_row,
            &widgets.sidebar.filtered_placement_row,
            &widgets.sidebar.tags_placement_row,
            &widgets.sidebar.favorites_placement_row,
            &widgets.sidebar.contacts_placement_row,
            &widgets.sidebar.start_view_row,
            &widgets.sidebar.folder_sort_row,
        ] {
            no_truncate(row);
        }

        // The install-command field is read-only: the row's pencil, which
        // says "type here", would be a lie.
    }

    /// Settings → General and Privacy: the Files install command, language,
    /// renderer and the privacy switches.
    pub(super) fn init_general_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        hide_edit_icon(widgets.system.nautilus_cmd_row.upcast_ref());

        // Language combo: the system's, then every catalogue shipped.
        let choices = language_choices();
        let lang_labels: Vec<&str> = choices.iter().map(|(l, _)| l.as_str()).collect();
        widgets
            .system
            .language_row
            .set_model(Some(&gtk::StringList::new(&lang_labels)));
        widgets.system.language_row.set_selected(
            choices
                .iter()
                .position(|(_, c)| *c == init.language)
                .unwrap_or(0) as u32,
        );

        widgets
            .system
            .renderer_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Automatic"),
                "Vulkan",
                "OpenGL",
                &i18n("Software"),
            ])));
        let renderer = crate::config::load_renderer();
        widgets
            .system
            .renderer_row
            .set_selected(RENDERERS.iter().position(|r| *r == renderer).unwrap_or(0) as u32);

        widgets
            .privacy
            .auto_remote_content_row
            .set_active(init.auto_remote_content);
        widgets
            .privacy
            .remote_ahead_row
            .set_active(init.remote_ahead);
        widgets
            .privacy
            .show_remote_banner_row
            .set_active(init.show_remote_banner);
        widgets
            .privacy
            .show_spoof_banner_row
            .set_active(init.show_spoof_banner);
        no_truncate(&widgets.privacy.receipts_row);
        widgets
            .privacy
            .receipts_row
            .set_selected(super::choices::receipts_index(init.receipts));
        widgets
            .composing
            .request_receipts_row
            .set_active(init.request_receipts);
        no_truncate(&widgets.composing.returned_receipts_row);
        widgets.composing.returned_receipts_row.set_selected(
            super::choices::returned_receipts_index(init.returned_receipts),
        );
        widgets.privacy.gravatar_row.set_active(init.gravatar);
        widgets.list.avatars_row.set_active(init.avatars);
        widgets
            .list
            .own_mailbox_face_row
            .set_active(init.own_mailbox_face);
        widgets.list.tag_row_tint_row.set_active(init.tag_row_tint);
        widgets
            .privacy
            .sender_logos_row
            .set_active(init.sender_logos);
    }

    /// Settings → Notifications: mail checks, alerts and the new-mail sound.
    pub(super) fn init_notification_rows(&mut self, widgets: &PreferencesWidgets, init: &PrefInit) {
        // Mail-check interval combo.
        let labels_owned: Vec<String> = FETCH_INTERVALS.iter().map(|(l, _)| i18n(l)).collect();
        let labels: Vec<&str> = labels_owned.iter().map(String::as_str).collect();
        widgets
            .general
            .fetch_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        let selected = FETCH_INTERVALS
            .iter()
            .position(|(_, secs)| *secs == init.fetch_interval_secs)
            .unwrap_or(0);
        widgets.general.fetch_row.set_selected(selected as u32);
        widgets.general.push_row.set_active(init.push);
        widgets
            .general
            .metered_row
            .set_active(init.metered_save_data);
        widgets
            .general
            .notifications_row
            .set_active(init.notifications);
        widgets
            .general
            .notification_content_row
            .set_active(init.notification_content);
        widgets
            .general
            .notify_receipts_row
            .set_active(init.notify_receipts);
        // Set before the handlers can act on it: the self already holds
        // these values, so the echo is ignored rather than played.
        widgets.general.sound_row.set_active(self.sound.enabled);
        let labels_owned: Vec<String> = sound_labels();
        let labels: Vec<&str> = labels_owned.iter().map(String::as_str).collect();
        widgets
            .general
            .sound_choice_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        widgets
            .general
            .sound_choice_row
            .set_selected(sound_index(&self.sound.sound));
        widgets
            .general
            .sound_focused_row
            .set_active(self.sound.when_focused);
        widgets
            .general
            .notify_mark_read_row
            .set_active(init.notification_buttons.mark_read);
        widgets
            .general
            .notify_archive_row
            .set_active(init.notification_buttons.archive);
        widgets
            .general
            .notify_delete_row
            .set_active(init.notification_buttons.delete);
        widgets
            .general
            .notify_reply_row
            .set_active(init.notification_buttons.reply);
        widgets
            .general
            .notify_forward_row
            .set_active(init.notification_buttons.forward);
        widgets
            .general
            .notify_spam_row
            .set_active(init.notification_buttons.spam);
        widgets
            .sidebar
            .show_attachments_row
            .set_active(init.show_attachments);
    }

    /// Settings → Sidebar: the unified section, placements, start view, folder
    /// order and the icon rail.
    pub(super) fn init_sidebar_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        widgets
            .sidebar
            .show_unified_row
            .set_active(init.show_unified);
        widgets
            .sidebar
            .unified_chip_all_inboxes_row
            .set_active(init.unified_chips.all_inboxes);
        widgets
            .sidebar
            .unified_chip_starred_row
            .set_active(init.unified_chips.starred);
        widgets
            .sidebar
            .unified_chip_drafts_row
            .set_active(init.unified_chips.drafts);
        widgets
            .sidebar
            .unified_chip_archive_row
            .set_active(init.unified_chips.archive);
        widgets
            .sidebar
            .unified_chip_junk_row
            .set_active(init.unified_chips.junk);
        widgets
            .sidebar
            .unified_chip_trash_row
            .set_active(init.unified_chips.trash);
        widgets
            .sidebar
            .unified_chip_snoozed_row
            .set_active(init.unified_chips.snoozed);
        widgets
            .sidebar
            .unified_starred_row
            .set_active(init.unified_kinds.starred);
        widgets
            .sidebar
            .unified_sent_row
            .set_active(init.unified_kinds.sent);
        widgets
            .sidebar
            .unified_drafts_row
            .set_active(init.unified_kinds.drafts);
        widgets
            .sidebar
            .unified_archive_row
            .set_active(init.unified_kinds.archive);
        widgets
            .sidebar
            .unified_junk_row
            .set_active(init.unified_kinds.junk);
        widgets
            .sidebar
            .unified_trash_row
            .set_active(init.unified_kinds.trash);
        widgets
            .sidebar
            .unified_snoozed_row
            .set_active(init.unified_kinds.snoozed);
        widgets
            .sidebar
            .section_style_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Unified style").as_str(),
                i18n("Tree style").as_str(),
            ])));
        widgets
            .sidebar
            .section_style_row
            .set_selected(section_style_index(init.section_style));
        for (row, placement) in [
            (
                &widgets.sidebar.filtered_placement_row,
                init.filtered_placement,
            ),
            (&widgets.sidebar.tags_placement_row, init.tags_placement),
            (
                &widgets.sidebar.favorites_placement_row,
                init.favorites_placement,
            ),
        ] {
            row.set_model(Some(&gtk::StringList::new(&[
                i18n("Above the accounts").as_str(),
                i18n("Below the accounts").as_str(),
                i18n("Off").as_str(),
            ])));
            row.set_selected(placement_index(placement));
        }
        widgets
            .sidebar
            .contacts_placement_row
            .set_model(Some(&contacts_placement_model()));
        widgets
            .sidebar
            .contacts_placement_row
            .set_selected(contacts_placement_index(init.contacts_placement));
        widgets
            .sidebar
            .start_view_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("All Inboxes").as_str(),
                i18n("The last account's inbox").as_str(),
                i18n("The last folder").as_str(),
            ])));
        widgets
            .sidebar
            .start_view_row
            .set_selected(start_view_index(init.start_view));
        let sort_labels: Vec<String> = crate::config::FolderSort::ALL
            .iter()
            .map(|s| s.label())
            .collect();
        let sort_labels: Vec<&str> = sort_labels.iter().map(String::as_str).collect();
        widgets
            .sidebar
            .folder_sort_row
            .set_model(Some(&gtk::StringList::new(&sort_labels)));
        widgets
            .sidebar
            .folder_sort_row
            .set_selected(init.folder_sort.index());
        widgets
            .sidebar
            .chevron_side_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Left").as_str(),
                i18n("Right").as_str(),
            ])));
        widgets
            .sidebar
            .chevron_side_row
            .set_selected(if init.chevrons_left { 0 } else { 1 });
        widgets
            .sidebar
            .account_click_row
            .set_active(init.account_click_folds);
        widgets
            .sidebar
            .sidebar_hover_expand_row
            .set_active(init.sidebar_hover_expand);
        widgets
            .sidebar
            .remember_sidebar_row
            .set_active(init.remember_sidebar);
        widgets
            .sidebar
            .remember_rail_row
            .set_active(init.remember_rail);
        widgets.sidebar.rail_dots_row.set_active(init.rail_dots);
        widgets.sidebar.seen_counts_row.set_active(init.seen_counts);
        widgets
            .sidebar
            .rail_fold_row
            .set_enable_expansion(init.rail_fold.enabled);
        // Unread counts and Fold up expanded items start folded each
        // launch and stay as they are left for the rest of the session,
        // a rebuilt Settings window included. Switching expansion on opens
        // a row, so its state is set after that.
        for (row, i) in [
            (&widgets.sidebar.unified_chips_row, 0),
            (&widgets.sidebar.rail_fold_row, 1),
        ] {
            row.set_expanded(SESSION_OPEN.with(|o| o.get()[i]));
            row.connect_expanded_notify(move |row| {
                SESSION_OPEN.with(|o| {
                    let mut open = o.get();
                    open[i] = row.is_expanded();
                    o.set(open);
                });
            });
        }
        widgets
            .sidebar
            .rail_fold_accounts_row
            .set_active(init.rail_fold.accounts);
        widgets
            .sidebar
            .rail_fold_all_inboxes_row
            .set_active(init.rail_fold.all_inboxes);
        widgets
            .sidebar
            .rail_fold_starred_row
            .set_active(init.rail_fold.starred);
        widgets
            .sidebar
            .rail_fold_sent_row
            .set_active(init.rail_fold.sent);
        widgets
            .sidebar
            .rail_fold_drafts_row
            .set_active(init.rail_fold.drafts);
        widgets
            .sidebar
            .rail_fold_archive_row
            .set_active(init.rail_fold.archive);
        widgets
            .sidebar
            .rail_fold_junk_row
            .set_active(init.rail_fold.junk);
        widgets
            .sidebar
            .rail_fold_trash_row
            .set_active(init.rail_fold.trash);
        widgets
            .sidebar
            .rail_fold_snoozed_row
            .set_active(init.rail_fold.snoozed);
        widgets
            .sidebar
            .rail_fold_filtered_row
            .set_active(init.rail_fold.filtered);
        widgets
            .sidebar
            .rail_fold_tags_row
            .set_active(init.rail_fold.tags);
    }

    /// Settings → Message List.
    pub(super) fn init_list_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        let preview_labels_owned = [
            i18n("Off"),
            i18n("1 line"),
            i18n("2 lines"),
            i18n("3 lines"),
        ];
        let preview_labels: Vec<&str> = preview_labels_owned.iter().map(String::as_str).collect();
        widgets
            .list
            .preview_lines_row
            .set_model(Some(&gtk::StringList::new(&preview_labels)));
        widgets
            .list
            .preview_lines_row
            .set_selected(init.preview_lines.min(3));
        widgets
            .list
            .list_layout_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Cards"),
                &i18n("Single line"),
                &i18n("Automatic"),
            ])));
        no_truncate(&widgets.list.list_layout_row);
        widgets
            .list
            .list_headings_row
            .set_active(init.list_headings);
        widgets
            .list
            .list_fit_row
            .set_active(!init.list_column_scroll);
        widgets
            .list
            .group_by_date_row
            .set_active(init.list_group_by_date);
        widgets
            .list
            .pills_below_row
            .set_active(init.list_pills_below);
        widgets
            .list
            .list_search_open_row
            .set_active(init.list_search_open);
        for (row, bottom) in [
            (&widgets.list.list_search_place_row, init.list_search_bottom),
            (&widgets.list.list_bulk_place_row, init.list_bulk_bar_bottom),
        ] {
            row.set_model(Some(&gtk::StringList::new(&[
                &i18n("Top of the list"),
                &i18n("Bottom of the list"),
            ])));
            row.set_selected(u32::from(bottom));
            no_truncate(row);
        }
        widgets
            .list
            .list_layout_row
            .set_selected(list_layout_index(init.list_layout));
    }

    /// Running in the background, then the theme gallery and the app-icon strip
    /// (the strip fills in on idle).
    pub(super) fn init_background_and_galleries(
        &self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
        sender: &ComponentSender<Self>,
    ) {
        widgets
            .system
            .background_row
            .set_active(init.run_in_background);
        widgets.system.autostart_row.set_active(init.autostart);
        widgets
            .system
            .fullscreen_row
            .set_active(init.start_fullscreen);
        // Starting at login only means anything if Hylki stays running.
        widgets
            .system
            .autostart_row
            .set_sensitive(init.run_in_background);
        {
            let autostart_row = widgets.system.autostart_row.clone();
            widgets
                .system
                .background_row
                .connect_active_notify(move |row| {
                    autostart_row.set_sensitive(row.is_active());
                });
        }
        // Theme: title in the row's own voice, the gallery beneath — the
        // same shape as the app-icon row under it. A pick goes straight
        // out and is painted at once.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(12);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("Theme").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            let s = sender.clone();
            let gallery = crate::ui::theme_picker::gallery(
                &init.theme,
                std::rc::Rc::new(move |id: &str| {
                    let _ = s.output(PrefOutput::SetTheme(id.to_string()));
                }),
            );
            gallery.set_margin_top(10);
            body.append(&gallery);
            widgets.appearance.theme_row.set_child(Some(&body));
        }

        // App icon: title in the row's own voice, the gallery beneath.
        // Picks go straight out; the app applies and offers the restart.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(8);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("App icon").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            // An icon set outside Hylki stays until a pick here (#252): say
            // so, and ring no tile, since none of them is what shows.
            let custom = crate::desktop::app_icon::custom_icon().is_some();
            let note = gtk::Label::new(Some(
                i18n("The launcher shows an icon set elsewhere. Picking one here replaces it.")
                    .as_str(),
            ));
            note.add_css_class("dim-label");
            note.add_css_class("caption");
            note.set_halign(gtk::Align::Start);
            note.set_xalign(0.0);
            note.set_wrap(true);
            note.set_visible(custom);
            body.append(&note);
            widgets.appearance.app_icon_row.set_child(Some(&body));
            // The icon gallery decodes the whole catalogue; it fills in a
            // moment after the window is up rather than holding it back.
            let s = sender.clone();
            let app_icon = if custom {
                String::new()
            } else {
                init.app_icon.clone()
            };
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                let s = s.clone();
                let note = note.clone();
                let strip = crate::ui::icon_picker::strip(
                    &app_icon,
                    56,
                    std::rc::Rc::new(move |id: &str| {
                        note.set_visible(false);
                        let _ = s.output(PrefOutput::SetAppIcon(id.to_string()));
                    }),
                );
                strip.set_margin_top(6);
                body.append(&strip);
                gtk::glib::ControlFlow::Break
            });
        }
    }

    /// The tray icon and the launcher badge.
    pub(super) fn init_tray_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        widgets.system.tray_row.set_active(init.tray);
        let tray_icon_labels_owned: Vec<String> = TRAY_ICONS.iter().map(|(l, _)| i18n(l)).collect();
        let tray_icon_labels: Vec<&str> =
            tray_icon_labels_owned.iter().map(String::as_str).collect();
        widgets
            .system
            .tray_icon_row
            .set_model(Some(&gtk::StringList::new(&tray_icon_labels)));
        let tray_icon_sel = TRAY_ICONS
            .iter()
            .position(|(_, t)| *t == init.tray_icon)
            .unwrap_or(0);
        widgets
            .system
            .tray_icon_row
            .set_selected(tray_icon_sel as u32);
        widgets.system.tray_mail_row.set_active(init.tray_mail);
        let badge_labels: Vec<String> = LAUNCHER_BADGES.iter().map(|(l, _)| i18n(l)).collect();
        let badge_labels: Vec<&str> = badge_labels.iter().map(String::as_str).collect();
        widgets
            .system
            .launcher_badge_row
            .set_model(Some(&gtk::StringList::new(&badge_labels)));
        let badge_sel = LAUNCHER_BADGES
            .iter()
            .position(|(_, b)| *b == init.launcher_badge)
            .unwrap_or(1);
        widgets
            .system
            .launcher_badge_row
            .set_selected(badge_sel as u32);
        // The icon choice and the menu's mail list only mean anything with
        // a tray icon shown.
        widgets.system.tray_icon_row.set_sensitive(init.tray);
        widgets.system.tray_mail_row.set_sensitive(init.tray);
        {
            let tray_icon_row = widgets.system.tray_icon_row.clone();
            let tray_mail_row = widgets.system.tray_mail_row.clone();
            widgets.system.tray_row.connect_active_notify(move |row| {
                tray_icon_row.set_sensitive(row.is_active());
                tray_mail_row.set_sensitive(row.is_active());
            });
        }
    }

    /// Keyboard shortcuts, downloaded mail and the console.
    pub(super) fn init_keyboard_and_system_rows(
        &self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
    ) {
        widgets
            .system
            .single_key_row
            .set_active(init.single_key_shortcuts);
        widgets.system.gmail_keys_row.set_active(init.gmail_keys);
        // Gmail's keys are a kind of single-key shortcut, so they mean
        // nothing with those off (#392): the switch keeps its state but
        // greys out, as the tray's own options do.
        widgets
            .system
            .single_key_row
            .bind_property("active", &widgets.system.gmail_keys_row, "sensitive")
            .sync_create()
            .build();
        widgets
            .system
            .keep_downloads_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Always"),
                &i18n("A year"),
                &i18n("90 days"),
                &i18n("30 days"),
            ])));
        widgets.system.keep_downloads_row.set_selected(
            KEEP_DOWNLOADS_DAYS
                .iter()
                .position(|d| *d == init.keep_downloads_days)
                .unwrap_or(0) as u32,
        );
        widgets
            .system
            .console_mode_row
            .set_active(init.console_mode);
        widgets
            .system
            .status_bar_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Bottom of the window"),
                &i18n("Top of the window"),
            ])));
        widgets
            .system
            .status_bar_row
            .set_selected(u32::from(init.status_bar_top));
        widgets
            .reading
            .read_mark_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("When displayed").as_str(),
                i18n("After two seconds").as_str(),
                i18n("Manually").as_str(),
            ])));
    }

    /// Date and clock styles, the app and message themes, and the text size.
    pub(super) fn init_date_and_theme_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        // Date and clock combos.
        let date_labels_owned: Vec<String> = DATE_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let date_labels: Vec<&str> = date_labels_owned.iter().map(String::as_str).collect();
        widgets
            .datetime
            .date_style_row
            .set_model(Some(&gtk::StringList::new(&date_labels)));
        widgets.datetime.date_style_row.set_selected(
            DATE_STYLES
                .iter()
                .position(|(_, s)| *s == init.date_style)
                .unwrap_or(0) as u32,
        );
        let clock_labels_owned: Vec<String> = CLOCK_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let clock_labels: Vec<&str> = clock_labels_owned.iter().map(String::as_str).collect();
        widgets
            .datetime
            .clock_style_row
            .set_model(Some(&gtk::StringList::new(&clock_labels)));
        widgets.datetime.clock_style_row.set_selected(
            CLOCK_STYLES
                .iter()
                .position(|(_, s)| *s == init.clock_style)
                .unwrap_or(0) as u32,
        );

        // Message-content appearance combo.
        let app_theme_labels_owned: Vec<String> = APP_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let app_theme_labels: Vec<&str> =
            app_theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .appearance
            .app_theme_row
            .set_model(Some(&gtk::StringList::new(&app_theme_labels)));
        let app_theme_sel = APP_THEMES
            .iter()
            .position(|(_, t)| *t == init.app_theme)
            .unwrap_or(0);
        widgets
            .appearance
            .app_theme_row
            .set_selected(app_theme_sel as u32);
        let text_scale_labels_owned: Vec<String> = crate::desktop::text_scale::STEPS
            .iter()
            .map(|p| match p {
                100 => i18n("Default"),
                p => i18n_f("{percent}%", &[("percent", &p.to_string())]),
            })
            .collect();
        let text_scale_labels: Vec<&str> =
            text_scale_labels_owned.iter().map(String::as_str).collect();
        widgets
            .appearance
            .text_scale_row
            .set_model(Some(&gtk::StringList::new(&text_scale_labels)));
        let text_scale_sel = crate::desktop::text_scale::STEPS
            .iter()
            .position(|p| *p == init.text_scale)
            .unwrap_or(1);
        widgets
            .appearance
            .text_scale_row
            .set_selected(text_scale_sel as u32);

        let theme_labels_owned: Vec<String> = MESSAGE_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let theme_labels: Vec<&str> = theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .reading
            .message_theme_row
            .set_model(Some(&gtk::StringList::new(&theme_labels)));
        let theme_sel = MESSAGE_THEMES
            .iter()
            .position(|(_, t)| *t == init.message_theme)
            .unwrap_or(0);
        widgets
            .reading
            .message_theme_row
            .set_selected(theme_sel as u32);
    }

    /// The reader's font and color overrides, and the swipe and palette timing
    /// rows.
    pub(super) fn init_reader_fonts_and_gestures(
        &self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
    ) {
        // The reader's font override (#56): the button shows the chosen font,
        // or the interface font when none was chosen yet (so what it shows is
        // what the reader will use); the row only responds while the switch
        // is on.
        {
            let desc = if init.reader_font.trim().is_empty() {
                gtk::Settings::default()
                    .and_then(|s| s.gtk_font_name())
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "Cantarell 11".to_string())
            } else {
                init.reader_font.clone()
            };
            widgets
                .reading
                .reader_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets
                .reading
                .reader_font_row
                .set_sensitive(init.override_fonts);
            let font_row = widgets.reading.reader_font_row.clone();
            widgets
                .reading
                .override_fonts_row
                .connect_active_notify(move |row| {
                    font_row.set_sensitive(row.is_active());
                });
        }
        widgets
            .reading
            .override_fonts_row
            .set_active(init.override_fonts);
        widgets
            .reading
            .override_colors_row
            .set_active(init.override_colors);
        // Plain-text messages in monospace (#181): the font button shows
        // the desktop's monospace font until another is chosen.
        {
            let desc = if init.plain_font.trim().is_empty() {
                crate::desktop::session::monospace_font()
            } else {
                init.plain_font.clone()
            };
            widgets
                .reading
                .plain_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets
                .reading
                .plain_font_row
                .set_sensitive(init.plain_monospace);
            let font_row = widgets.reading.plain_font_row.clone();
            widgets
                .reading
                .plain_mono_row
                .connect_active_notify(move |row| {
                    font_row.set_sensitive(row.is_active());
                });
        }
        widgets
            .reading
            .plain_mono_row
            .set_active(init.plain_monospace);

        // Trackpad swipe sensitivity: 1 (libadwaita's own, far too long a
        // swipe for most trackpads) to 10, in half steps.
        let adj = gtk::Adjustment::new(
            init.swipe_sensitivity,
            crate::config::SWIPE_SENSITIVITY_MIN,
            crate::config::SWIPE_SENSITIVITY_MAX,
            0.5,
            1.0,
            0.0,
        );
        widgets.list.swipe_sensitivity_row.set_digits(1);
        widgets
            .list
            .swipe_sensitivity_row
            .set_adjustment(Some(&adj));

        // Hover-palette delay spinner (0–3000ms, step 50).
        // Actions palette timeout: 1–30 seconds.
        let adj = gtk::Adjustment::new(init.palette_collapse_secs as f64, 1.0, 30.0, 1.0, 5.0, 0.0);
        widgets.list.palette_collapse_row.set_adjustment(Some(&adj));
        let adj = gtk::Adjustment::new(
            init.card_palette_collapse_secs as f64,
            1.0,
            30.0,
            1.0,
            5.0,
            0.0,
        );
        widgets
            .reading
            .card_palette_collapse_row
            .set_adjustment(Some(&adj));

        widgets
            .appearance
            .settings_open_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Settings").as_str(),
                i18n("Accounts").as_str(),
            ])));
        widgets
            .appearance
            .settings_open_row
            .set_selected(if init.settings_open_accounts { 1 } else { 0 });
    }
}
