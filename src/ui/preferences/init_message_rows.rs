//! Filling the reading, files and composing rows when the window is built.

use super::choices::{
    compose_format_index, fold_messages_index, read_mark_index, reader_default_index,
    reply_position_index, signature_position_index,
};
use super::rows::no_truncate;
use super::*;

impl Preferences {
    /// Settings → Conversations and Reading, and Printing.
    pub(super) fn init_conversation_and_reading_rows(
        &self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
    ) {
        no_truncate(&widgets.reading.read_mark_row);
        widgets
            .reading
            .read_mark_row
            .set_selected(read_mark_index(init.read_mark));
        widgets
            .conversations
            .threading_row
            .set_active(init.threading);
        widgets
            .conversations
            .threads_expanded_row
            .set_active(init.threads_expanded);
        widgets
            .conversations
            .thread_newest_first_row
            .set_active(init.thread_newest_first);
        widgets
            .conversations
            .fold_messages_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Never"),
                &i18n("Messages already read"),
                &i18n("All but the newest"),
            ])));
        no_truncate(&widgets.conversations.fold_messages_row);
        widgets
            .conversations
            .fold_messages_row
            .set_selected(fold_messages_index(init.fold_messages));
        widgets
            .reading
            .always_show_recipients_row
            .set_active(init.always_show_recipients);
        widgets.reading.pgp_labels_row.set_active(init.pgp_labels);
        let find_place = &widgets.reading.find_place_row;
        find_place.set_model(Some(&gtk::StringList::new(&[
            &i18n("Top of the message"),
            &i18n("Bottom of the message"),
        ])));
        find_place.set_selected(u32::from(init.reader_find_bottom));
        no_truncate(find_place);
        widgets
            .reading
            .print_page_numbers_row
            .set_active(init.print_options.page_numbers);
        widgets
            .reading
            .print_date_row
            .set_active(init.print_options.date);
        widgets
            .reading
            .print_punch_margin_row
            .set_active(init.print_options.punch_margin);
        widgets
            .reading
            .single_message_card_row
            .set_active(init.single_message_card);
        widgets
            .reading
            .reader_switch_row
            .set_active(init.reader_switch);
        widgets
            .reading
            .theme_switch_row
            .set_active(init.theme_switch);
        widgets
            .appearance
            .desktop_icons_row
            .set_active(init.desktop_icons);
        widgets
            .appearance
            .toolbar_fold_row
            .set_active(init.toolbar_fold_right);
        widgets
            .reading
            .reader_default_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Remember the last choice"),
                &i18n("Reader View on"),
                &i18n("Reader View off"),
            ])));
        no_truncate(&widgets.reading.reader_default_row);
        widgets
            .reading
            .reader_default_row
            .set_selected(reader_default_index(init.reader_default));
        let zoom_labels: Vec<String> = crate::config::READER_ZOOM_STEPS
            .iter()
            .map(|z| format!("{z}%"))
            .collect();
        let zoom_labels: Vec<&str> = zoom_labels.iter().map(String::as_str).collect();
        widgets
            .reading
            .reader_zoom_row
            .set_model(Some(&gtk::StringList::new(&zoom_labels)));
        no_truncate(&widgets.reading.reader_zoom_row);
        widgets.reading.reader_zoom_row.set_selected(
            crate::config::READER_ZOOM_STEPS
                .iter()
                .position(|&z| z == init.reader_zoom)
                .unwrap_or(5) as u32,
        );
        widgets
            .conversations
            .card_attachments_row
            .set_active(init.card_attachments);
        widgets
            .conversations
            .attachment_drawer_row
            .set_active(init.attachment_drawer);
        widgets
            .conversations
            .thread_expansion_row
            .set_active(init.thread_expansion);
        widgets
            .conversations
            .thread_row_newest_row
            .set_active(init.thread_row_newest);
        widgets
            .conversations
            .confirm_thread_delete_row
            .set_active(init.confirm_thread_delete);
        widgets
            .reading
            .card_actions_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Hidden behind a toggle").as_str(),
                i18n("Shown while hovering").as_str(),
                i18n("Always visible").as_str(),
            ])));
        widgets
            .reading
            .card_actions_row
            .set_selected(if init.card_actions_hover {
                0
            } else if init.card_actions_auto {
                1
            } else {
                2
            });
        widgets.list.list_palette_row.set_active(init.list_palette);
        widgets
            .list
            .list_palette_hover_row
            .set_active(init.list_palette_hover);
        widgets
            .reading
            .card_palette_menu_row
            .set_active(init.card_palette_menu);
        widgets
            .list
            .swipe_enabled_row
            .set_active(init.swipe_enabled);
        widgets
            .list
            .swipe_reversed_row
            .set_active(init.swipe_reversed);
        widgets
            .composing
            .compose_inline_row
            .set_active(init.compose_inline);
        widgets
            .composing
            .reply_inline_row
            .set_active(init.reply_inline);
        widgets
            .composing
            .reply_fields_row
            .set_active(init.reply_fields);
    }

    /// Settings → System → Files and Links.
    pub(super) fn init_files_and_links_rows(
        &mut self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
        sender: &ComponentSender<Self>,
    ) {
        widgets
            .system
            .files_action_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Ask each time").as_str(),
                i18n("A new message").as_str(),
                i18n("A draft you pick").as_str(),
                i18n("A reply to a message you pick").as_str(),
            ])));
        no_truncate(&widgets.system.files_action_row);
        widgets
            .system
            .files_action_row
            .set_selected(files_action_index(init.files.action));
        widgets
            .system
            .files_large_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Ask each time").as_str(),
                i18n("Attach them anyway").as_str(),
                i18n("Upload to cloud storage and link them").as_str(),
            ])));
        no_truncate(&widgets.system.files_large_row);
        widgets
            .system
            .files_large_row
            .set_selected(files_large_index(init.files.large));
        widgets
            .system
            .files_limit_row
            .set_value(init.files.limit_mb as f64);
        self.files_rows = Some((
            widgets.system.files_action_row.clone(),
            widgets.system.files_large_row.clone(),
            widgets.system.files_limit_row.clone(),
        ));
        // The links combo: the desktop's own default, the chooser, then every
        // browser this process can launch.
        {
            let mut labels: Vec<String> = vec![i18n("System default"), i18n("Ask each time")];
            labels.extend(self.browsers.iter().map(|b| b.name.clone()));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            widgets
                .system
                .link_browser_row
                .set_model(Some(&gtk::StringList::new(&refs)));
            no_truncate(&widgets.system.link_browser_row);
            widgets
                .system
                .link_browser_row
                .set_selected(link_browser_index(&self.browsers, &init.link_browser));
            widen_combo_value(&widgets.system.link_browser_row, 50);
            let s = sender.clone();
            widgets
                .system
                .link_browser_row
                .connect_selected_notify(move |row| {
                    s.input(PrefInput::ChangeLinkBrowser(row.selected()));
                });
        }
    }

    /// Settings → Composing, spelling included.
    pub(super) fn init_composing_rows(
        &mut self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
        sender: &ComponentSender<Self>,
    ) {
        // "Send new messages from": the open folder's account, then every
        // enabled account and alias, labelled as the composer's From row
        // labels them. Only meaningful with more than one identity.
        {
            let mut labels: Vec<String> = vec![i18n("Account of the current folder")];
            labels.extend(self.identities.iter().map(|(name, addr)| {
                if name.trim().is_empty() {
                    addr.clone()
                } else {
                    format!("{name} <{addr}>")
                }
            }));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            widgets
                .composing
                .default_from_row
                .set_model(Some(&gtk::StringList::new(&refs)));
            let want = init.compose_default_from.trim();
            let sel = self
                .identities
                .iter()
                .position(|(_, addr)| addr.eq_ignore_ascii_case(want))
                .map(|i| i + 1)
                .unwrap_or(0);
            widgets.composing.default_from_row.set_selected(sel as u32);
            widgets
                .composing
                .default_from_row
                .set_visible(self.identities.len() > 1);
            middle_ellipsize(&widgets.composing.default_from_row);
        }
        widgets
            .composing
            .paste_plain_row
            .set_active(init.paste_plain);
        widgets
            .composing
            .return_paragraph_row
            .set_active(init.return_paragraph);
        // "Always collapsed" in full, not ellipsized beside the description.
        widgets
            .composing
            .toolbar_row
            .set_factory(Some(&crate::ui::accounts::non_ellipsizing_factory()));
        widgets
            .composing
            .toolbar_row
            .set_selected(u32::from(init.toolbar_expanded));
        widgets
            .composing
            .compose_format_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Rich text"),
                &i18n("Markdown"),
                &i18n("HTML source"),
                &i18n("Plain text"),
            ])));
        no_truncate(&widgets.composing.compose_format_row);
        widgets
            .composing
            .compose_format_row
            .set_selected(compose_format_index(init.compose_format));
        compose_font_setup(
            &widgets.composing.compose_font_row,
            &init.compose_font,
            sender.clone(),
        );
        {
            let row = &widgets.composing.compose_size_row;
            let sizes = crate::ui::rich_editor::SIZES;
            let names: Vec<String> = std::iter::once(i18n("Default"))
                .chain(sizes.iter().map(|pt| pt.to_string()))
                .collect();
            row.set_model(Some(&gtk::StringList::new(
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            let at = sizes
                .iter()
                .position(|pt| *pt == init.compose_font_size)
                .map_or(0, |i| i + 1);
            row.set_selected(at as u32);
            let sender = sender.clone();
            row.connect_selected_notify(move |row| {
                let pt = row
                    .selected()
                    .checked_sub(1)
                    .and_then(|i| sizes.get(i as usize))
                    .copied()
                    .unwrap_or(0);
                let _ = sender.output(PrefOutput::SetComposeFontSize(pt));
            });
        }
        widgets
            .composing
            .follow_plain_row
            .set_active(init.follow_plain);
        widgets
            .composing
            .signature_position_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Above the quoted message"),
                &i18n("Below the quoted message"),
            ])));
        no_truncate(&widgets.composing.signature_position_row);
        widgets
            .composing
            .signature_position_row
            .set_selected(signature_position_index(init.signature_position));
        widgets
            .composing
            .signature_dashes_row
            .set_active(init.signature_dashes);
        widgets.composing.set_reply_header(&init.reply_header);
        let undo_labels: Vec<String> = crate::config::UNDO_SEND_CHOICES
            .iter()
            .map(|&secs| match secs {
                0 => i18n("Off"),
                n => ni18n_f("{n} second", "{n} seconds", n, &[("n", &n.to_string())]),
            })
            .collect();
        let undo_labels: Vec<&str> = undo_labels.iter().map(String::as_str).collect();
        widgets
            .composing
            .undo_send_row
            .set_model(Some(&gtk::StringList::new(&undo_labels)));
        no_truncate(&widgets.composing.undo_send_row);
        // A delay set by hand in the file shows as the nearest one offered.
        let nearest = crate::config::UNDO_SEND_CHOICES
            .iter()
            .enumerate()
            .min_by_key(|(_, &secs)| secs.abs_diff(init.undo_send_secs))
            .map_or(0, |(i, _)| i as u32);
        widgets.composing.undo_send_row.set_selected(nearest);
        widgets
            .conversations
            .reply_position_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Above the messages"),
                &i18n("Below the messages"),
                &i18n("Follows the reading order"),
            ])));
        no_truncate(&widgets.conversations.reply_position_row);
        widgets
            .conversations
            .reply_position_row
            .set_selected(reply_position_index(init.reply_position));
        widgets.composing.spellcheck_row.set_active(init.spellcheck);
        // The language switches offer exactly what checking can use: the
        // installed dictionaries, behind a "System language" default. Typed
        // codes are gone — a language nobody has a dictionary for silently
        // checks nothing, so only real options are offered (#114). Any
        // number of them can be on at once (#365).
        {
            let rows = SpellLangRows::new(
                widgets.composing.spell_lang_row.clone(),
                init.spellcheck_langs.clone(),
            );
            self.spell_langs = Some(rows.clone());
            // Listing the installed dictionaries reads several directories;
            // done a moment after the window is up, not before it.
            let spelling_group = widgets.composing.spelling_group.clone();
            let sender = sender.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                let dicts = crate::desktop::spell::installed_dictionaries();
                let system = adw::SwitchRow::builder()
                    .title(i18n("System language"))
                    .subtitle(gtk::glib::markup_escape_text(
                        &crate::desktop::spell::spell_language_names(
                            &crate::desktop::spell::system_spell_languages(),
                        ),
                    ))
                    .build();
                rows.expander.add_row(&system);
                *rows.system.borrow_mut() = Some(system.clone());
                for d in &dicts {
                    let row = adw::SwitchRow::builder()
                        .title(gtk::glib::markup_escape_text(
                            &crate::desktop::spell::language_display_name(d),
                        ))
                        .build();
                    rows.expander.add_row(&row);
                    rows.dicts.borrow_mut().push((d.clone(), row));
                }
                let current = rows.setting.borrow().clone();
                rows.show(&current);
                if dicts.is_empty() {
                    rows.expander.set_sensitive(false);
                    spelling_group.set_description(Some(&i18n(
                        "No dictionaries found. On Flatpak, add a language with: flatpak config --set extra-languages <code>",
                    )));
                }
                // Connected after the switches show the saved choice, and
                // guarded besides, so showing it never saves it again.
                let switches = std::iter::once((String::new(), system))
                    .chain(rows.dicts.borrow().iter().cloned())
                    .collect::<Vec<_>>();
                for (code, row) in switches {
                    let (rows, s) = (rows.clone(), sender.clone());
                    row.connect_active_notify(move |row| {
                        if let Some(next) = rows.toggled(&code, row.is_active()) {
                            let _ = s.output(PrefOutput::SetSpellcheckLangs(next));
                        }
                    });
                }
                gtk::glib::ControlFlow::Break
            });
        }
        {
            // The learned-words list manages itself entirely in widget-land:
            // the rows call the spell module directly and rebuild in place.
            let exp = widgets.composing.spell_words_row.clone();
            let add = adw::EntryRow::builder().title(i18n("Add a word")).build();
            add.set_show_apply_button(true);
            {
                let exp = exp.clone();
                add.connect_apply(move |row| {
                    crate::desktop::spell::add_personal_word(&row.text());
                    row.set_text("");
                    rebuild_personal_words(&exp);
                });
            }
            exp.add_row(&add);
            rebuild_personal_words(&exp);
        }
    }
}
