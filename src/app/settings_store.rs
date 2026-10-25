//! Saving the app's settings to disk.

use relm4::prelude::*;

use crate::config;
use crate::ui::contacts_page::ContactsPageInput;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::sidebar::SidebarInput;

use super::model::AppModel;

impl AppModel {
    /// Persist all app settings together.
    pub(super) fn save_settings(&self) {
        config::save_privacy(config::PrivacyFile {
            allowed_senders: self.allowed_senders.clone(),
            auto_remote_content: self.auto_remote_content,
            gravatar: self.gravatar,
            avatars: self.avatars,
            own_mailbox_face: self.own_mailbox_face,
            contact_book: self.contact_book.clone(),
            tag_row_tint: self.tag_row_tint,
            sender_logos: self.sender_logos,
            date_style: self.date_style,
            clock_style: self.clock_style,
            fetch_interval_secs: self.fetch_interval_secs,
            push: self.push,
            metered_save_data: self.metered_save_data,
            blacklist: self.blacklist.clone(),
            palette_collapse_secs: self.palette_collapse_secs,
            card_palette_collapse_secs: self.card_palette_collapse_secs,
            threading: self.threading,
            threads_expanded: self.threads_expanded,
            thread_expansion: self.thread_expansion,
            thread_row_newest: self.thread_row_newest,
            thread_newest_first: self.thread_newest_first,
            fold_messages: self.fold_messages,
            always_show_recipients: self.always_show_recipients,
            pgp_labels: self.pgp_labels,
            reader_find_bottom: self.reader_find_bottom,
            single_message_card: self.single_message_card,
            reader_mode: self.reader_mode,
            reader_switch: self.reader_switch,
            theme_switch: self.theme_switch,
            desktop_icons: self.desktop_icons,
            reader_default: self.reader_default,
            reader_zoom: self.zoom_default,
            card_attachments: self.card_attachments,
            attachment_drawer: self.drawer_enabled,
            confirm_thread_delete: self.confirm_thread_delete,
            message_theme: self.message_theme,
            override_fonts: self.override_fonts,
            reader_font: self.reader_font.clone(),
            override_colors: self.override_colors,
            plain_monospace: self.plain_monospace,
            plain_font: self.plain_font.clone(),
            notifications: self.notifications_enabled,
            notification_content: self.notification_content,
            notification_buttons: self.notification_buttons.to_list(),
            show_attachments: self.show_attachments,
            settings_open_accounts: self.settings_open_accounts,
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
            compose_default_from: self.compose_default_from.clone(),
            paste_plain: self.paste_plain,
            return_paragraph: self.return_paragraph,
            toolbar_expanded: self.toolbar_expanded,
            compose_font: self.compose_font.clone(),
            compose_font_size: self.compose_font_size,
            print: self.print_options,
            // Both are written: the boolean is what an older version reads.
            compose_plain: self.compose_format == config::ComposeFormat::Plain,
            compose_format: Some(self.compose_format),
            follow_plain: self.follow_plain,
            reply_position: self.reply_position,
            signature_position: self.signature_position,
            signature_dashes: self.signature_dashes,
            reply_header: self.reply_header.clone(),
            undo_send_secs: self.undo_send_secs,
            spellcheck: self.spellcheck,
            // Every save is after the first load, which applied it.
            single_card_default_applied: true,
            spellcheck_langs: self.spellcheck_langs.clone(),
            preview_lines: self.preview_lines,
            list_layout: self.list_layout,
            list_columns: config::ListColumn::to_keys(&self.list_columns),
            list_headings: self.list_headings,
            list_column_scroll: self.list_column_scroll,
            list_group_by_date: self.list_group_by_date,
            list_pills_below: self.list_pills_below,
            date_groups_default_applied: true,
            list_search_open: self.list_search_open,
            contacts_search_open: self.contacts_search_open,
            list_search_bottom: self.list_search_bottom,
            list_bulk_bar_bottom: self.list_bulk_bar_bottom,
            toolbar_fold_right: self.toolbar_fold_right,
            list_column_widths: config::list_column_widths_to(&self.list_column_widths),
            single_key_shortcuts: self.single_key.get(),
            gmail_keys: self.gmail_keys.get(),
            keep_downloads_days: self.keep_downloads_days,
            run_in_background: self.run_in_background.get(),
            autostart: self.autostart,
            start_fullscreen: self.start_fullscreen,
            tray: self.tray_enabled,
            tray_icon: self.tray_icon,
            tray_mail: self.tray_mail,
            launcher_count: self.launcher_badge != config::LauncherBadge::Off,
            launcher_badge: Some(self.launcher_badge),
            show_remote_banner: self.show_remote_banner,
            show_spoof_banner: self.show_spoof_banner,
            sidebar_hover_expand: self.sidebar_hover_expand,
            remember_sidebar: self.remember_sidebar,
            remember_rail: self.remember_rail,
            rail_dots: self.rail_dots,
            seen_counts: self.seen_counts,
            rail_fold: self.rail_fold,
            app_theme: self.app_theme,
            text_scale: self.text_scale,
            theme: self.theme.clone(),
            show_unified: self.show_unified_pref,
            unified_chip: self.unified_chips.all_inboxes,
            unified_chips: self.unified_chips,
            unified_kinds: self.unified_kinds,
            show_accounts: self.show_accounts,
            section_style: self.section_style,
            filtered_placement: self.filtered_placement,
            tags_placement: self.tags_placement,
            favorites_placement: self.favorites_placement,
            contacts_placement: self.contacts_placement,
            contacts_footer_default_applied: true,
            chevrons_left: self.chevrons_left,
            account_click_folds: self.account_click_folds,
            console_mode: self.console_mode,
            status_bar_top: self.status_bar_top,
            read_mark: self.read_mark,
            files_action: self.files_prefs.action,
            files_large: self.files_prefs.large,
            files_limit_mb: self.files_prefs.limit_mb,
            link_browser: self.link_browser.clone(),
            start_view: self.start_view,
            folder_sort: self.folder_sort,
        });
    }

    pub(super) fn push_settings_to_children(&self) {
        self.message_view
            .emit(MessageViewInput::SetReadMark(self.read_mark));
        self.message_list
            .emit(MessageListInput::SetGravatar(self.gravatar));
        self.message_list
            .emit(MessageListInput::SetAvatars(self.list_avatars()));
        // The formatter is a free function, so the preference has to be handed to
        // it before anything draws a date.
        self.message_list
            .emit(MessageListInput::SetSenderLogos(self.sender_logos));
        crate::datefmt::set_style(self.date_style, self.clock_style);
        crate::ui::print_preview::set_options(self.print_options);
        self.message_list.emit(MessageListInput::SetLook {
            avatars: self.list_avatars(),
            preview_lines: self.list_preview_lines(),
            subject: self.list_show_subject(),
            animate: false,
        });
        self.message_view.emit(MessageViewInput::SetFoldLook {
            preview_lines: self.list_preview_lines(),
        });
        self.push_single_line();
        self.message_list
            .emit(MessageListInput::SetColumns(self.list_columns.clone()));
        self.message_list
            .emit(MessageListInput::SetHeadings(self.list_headings));
        self.message_list
            .emit(MessageListInput::SetColumnScroll(self.list_column_scroll));
        self.message_list
            .emit(MessageListInput::SetSearchPinned(self.list_search_open));
        self.push_list_bars();
        self.contacts_page.emit(ContactsPageInput::SetSearchPinned(
            self.contacts_search_open,
        ));
        self.contacts_page.emit(ContactsPageInput::SetBookPicker(
            !self.contacts_placement.shown(),
        ));
        self.message_list.emit(MessageListInput::SetColumnWidths(
            self.list_column_widths.clone(),
        ));
        self.sidebars_emit(SidebarInput::SetFocus {
            hide_accounts: self.focus.active(config::FocusPart::HideAccounts),
            fold_unified: self.focus.active(config::FocusPart::FoldUnified),
            animate: false,
        });
        self.message_list
            .emit(MessageListInput::SetThreading(self.threading));
        self.message_list
            .emit(MessageListInput::SetThreadExpansion(self.thread_expansion));
        self.message_list
            .emit(MessageListInput::SetThreadRowNewest(self.thread_row_newest));
        self.message_list
            .emit(MessageListInput::SetListPalette(self.list_palette));
        self.message_list
            .emit(MessageListInput::SetThreadsExpanded(self.threads_expanded));
        self.message_list.emit(MessageListInput::SetPaletteCollapse(
            self.palette_collapse_secs,
        ));
        self.message_view.emit(MessageViewInput::SetContentTheme(
            self.message_theme.dark_override(),
        ));
        self.message_view
            .emit(MessageViewInput::SetReaderStyle(self.reader_style()));
        self.message_view
            .emit(MessageViewInput::SetAlwaysShowRecipients(
                self.always_show_recipients,
            ));
        self.message_view
            .emit(MessageViewInput::SetPgpLabels(self.pgp_labels));
        self.message_view
            .emit(MessageViewInput::SetFindBottom(self.reader_find_bottom));
        self.message_view
            .emit(MessageViewInput::SetSingleMessageCard(
                self.single_message_card,
            ));
        self.message_view
            .emit(MessageViewInput::SetFoldMessages(self.fold_messages));
        self.message_view.emit(MessageViewInput::SetReaderMode(
            self.effective_reader_mode(),
        ));
        self.message_view
            .emit(MessageViewInput::SetZoomDefault(self.zoom_default));
        self.message_view.emit(MessageViewInput::SetZoom(self.zoom));
        self.message_view
            .emit(MessageViewInput::SetReaderSwitchShown(self.reader_switch));
        self.message_view
            .emit(MessageViewInput::SetThemeSwitchShown(self.theme_switch));
        self.message_view.emit(MessageViewInput::SetReaderDefault(
            self.effective_reader_default(),
        ));
        self.message_view
            .emit(MessageViewInput::SetCardAttachmentsShown(
                self.card_attachments,
            ));
        self.message_view
            .emit(MessageViewInput::SetAttachmentDrawer(self.drawer_enabled));
        self.message_view
            .emit(MessageViewInput::SetUnsubscribed(self.unsubscribed_map()));
        self.message_view
            .emit(MessageViewInput::SetIdentities(self.identities_map()));
        self.message_view.emit(MessageViewInput::SetInviteAnswers(
            self.invite_answers_map(),
        ));
    }
}
