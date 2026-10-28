//! The root component's model as it starts: every field's first value,
//! from what the boot steps read and the children they launched.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n;
use crate::models::FolderKind;
use crate::ui::message_list::QuickFilters;

use super::boot::{Boot, MainMenu};
use super::children::Children;
use super::consts::{ATTACHMENT_CACHE_BUDGET, BODY_CACHE_BUDGET};
use super::demo::{demo_account_configs_saved, demo_filters, demo_mode, demo_tags};
use super::list_header::LIST_WIDE_ON;
use super::model::AppModel;
use super::transfer::TransferTally;
use super::types::UnifiedView;

impl AppModel {
    /// Builds the model. Several fields read their own settings file here,
    /// and a few have side effects (the link browser handed to the
    /// launcher, the app icon set up, the demo cache seeded): the literal
    /// keeps its field order, which is the order those happen in.
    pub(super) fn new(
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
        boot: Boot,
        children: Children,
        main_menu: MainMenu,
    ) -> Self {
        let Boot {
            prefs,
            sidebar_state,
            focus,
            icon_only,
            rail_now,
            config,
            demo_data,
        } = boot;
        let config::SidebarState {
            order,
            collapsed,
            folders_expanded,
            icon_only: _,
            tree_collapsed,
            folder_order,
            unified_expanded,
            filtered_expanded,
            tags_expanded,
            starred_expanded,
            sent_expanded,
            drafts_expanded,
            archive_expanded,
            junk_expanded,
            trash_expanded,
            snoozed_expanded,
            favorites,
            favorites_expanded,
            smart_expanded,
            contacts_expanded,
            contacts_folded,
            filtered_expanded_accounts,
            tags_expanded_accounts,
        } = sidebar_state;
        let Children {
            sidebar,
            peek_sidebar,
            message_list,
            message_view,
            attachment_drawer,
            lightbox,
            gallery,
            contacts_compose_revealer,
            contacts_page,
            notifications,
        } = children;
        let MainMenu {
            menu,
            help: help_menu,
            undo: model_undo_menu,
            show_accounts_action,
            focus_action,
        } = main_menu;
        crate::ui::keymap::set_key_mode(prefs.single_key_shortcuts, prefs.gmail_keys);
        let launcher_badge = prefs.launcher_badge();
        let remember_sidebar = prefs.remember_sidebar;
        let remember_rail = prefs.remember_rail;
        let show_attachments = prefs.show_attachments;
        let start_view = prefs.start_view;
        let show_accounts = prefs.show_accounts;
        let reader_override = config::load_reader_override();
        let plain_style = config::load_plain_style();
        AppModel {
            workers: HashMap::new(),
            mid_searches: HashMap::new(),
            tag_scan: None,
            tag_scan_gen: 0,
            demo_config: if demo_mode() && config.is_empty() {
                demo_account_configs_saved()
            } else {
                Vec::new()
            },
            config,
            window: root.clone(),
            prefs: None,
            accounts_win: None,
            accounts_seed: None,
            composers: Vec::new(),
            reader_compose: None,
            send_morph: crate::ui::send_morph::SendMorph::new(),
            arriving: None,
            morph_pending: None,
            self_sender: sender.input_sender().clone(),
            provisionals: HashMap::new(),
            sent_files: HashMap::new(),
            draining_composers: Vec::new(),
            reader_split_top: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            reader_split_bottom: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideUp);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            reader_split_lower: {
                let p = gtk::Paned::new(gtk::Orientation::Vertical);
                // Same invisible separator as the outer split: the bottom
                // panel's grab pill is its visible affordance too.
                p.add_css_class("reader-split");
                p
            },
            split_close_anim: std::rc::Rc::new(std::cell::RefCell::new(None)),
            reader_header: std::cell::OnceCell::new(),
            toasts: std::cell::OnceCell::new(),
            outgoing: Default::default(),
            send_ticket: 0,
            reader_split: {
                let p = gtk::Paned::new(gtk::Orientation::Vertical);
                // The separator itself is styled invisible — painted the
                // composer's own ground, so the panel reads as running to its
                // edge. The visible affordance is the floating grab pill the
                // split slot overlays on the composer (open_inline_reply).
                p.add_css_class("reader-split");
                p
            },
            window_drop_zones: None,
            reader_compose_revealer: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            contacts_compose_revealer,
            next_compose_id: 1,
            menu,
            help_menu,
            status_bar_shown: false,
            undo_menu: model_undo_menu.clone(),
            accounts: Vec::new(),
            folders: HashMap::new(),
            account_order: order,
            collapsed,
            folders_expanded,
            tree_collapsed,
            folder_order,
            selected: None,
            attachments: Vec::new(),
            lightbox,
            attachments_loading: false,
            reader_actions_collapsed: false,
            reader_compact: false,
            reader_toolbar: config::load_reader_toolbar(),
            reader_toolbar_widgets: std::cell::OnceCell::new(),
            reader_overflow_btn: {
                let b = gtk::Button::from_icon_name("view-more-horizontal-symbolic");
                b.set_tooltip_text(Some(i18n("Actions").as_str()));
                b.add_css_class("flat");
                b.set_visible(false);
                b
            },
            reader_tag_btn: {
                let b = gtk::Button::from_icon_name("tag-outline-symbolic");
                b.set_tooltip_text(Some(i18n("Tags").as_str()));
                b.add_css_class("flat");
                b
            },
            reader_snooze_btn: {
                let b = gtk::Button::from_icon_name("alarm-symbolic");
                b.set_tooltip_text(Some(i18n("Snooze").as_str()));
                b.add_css_class("flat");
                b
            },
            reader_move_btn: {
                let b = gtk::Button::from_icon_name("folder-symbolic");
                b.set_tooltip_text(Some(i18n("Move To…").as_str()));
                b.add_css_class("flat");
                b
            },
            attachment_cache: crate::ram_cache::RamCache::new(ATTACHMENT_CACHE_BUDGET),
            unified: false,
            unified_view: UnifiedView::Kind(FolderKind::Inbox),
            unified_account: None,
            unified_slices: HashMap::new(),
            unified_boot_requested: HashSet::new(),
            sent_index_requested: HashSet::new(),
            message_cache: HashMap::new(),
            search_gen: 0,
            server_search: Default::default(),
            indexed_folders: HashSet::new(),
            body_cache: crate::ram_cache::RamCache::new(BODY_CACHE_BUDGET),
            sender_cache: HashMap::new(),
            pending_draft: None,
            pending_draft_token: None,
            pending_reply: None,
            pending_draft_pick: None,
            pending_template_pick: None,
            pending_edit_as_new: None,
            pending_forward: None,
            pending_notified_reply: None,
            files_prefs: config::load_files_prefs(),
            link_browser: {
                // The launcher reads its choice from here, not from disk, so
                // it is handed over before the first link can be clicked.
                let choice = prefs.link_browser;
                crate::ui::launch::set_browser(&choice);
                choice
            },
            popouts: HashMap::new(),
            current_thread: Vec::new(),
            listed_thread: Vec::new(),
            reader_focus_pending: false,
            list_selection: Vec::new(),
            list_slots: Vec::new(),
            checked: None,
            compose_waiting: false,
            compose_now: false,
            list_heard: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            compose_history: (None, None),
            carried_bodies: HashMap::new(),
            remote_override: HashMap::new(),
            carried_threads: HashMap::new(),
            undo_action: None,
            redo_action: None,
            bulk_pending: 0,
            related_id_seq: u32::MAX,
            related_ids: HashMap::new(),
            folder_unread: HashMap::new(),
            pending_seen: HashMap::new(),
            pending_tags: HashMap::new(),
            pending_answers: HashMap::new(),
            dropped_unread: Default::default(),
            pending_moves: std::cell::RefCell::new(HashMap::new()),
            transfers: HashMap::new(),
            next_transfer: 0,
            next_local_id: u32::MAX - 1,
            pending_saves: HashMap::new(),
            transfer_tally: TransferTally::default(),
            sidebar_split: None,
            app_title: None,
            sidebar_menu: None,
            sidebar_header: None,
            sidebar_refresh: {
                let b = gtk::Button::new();
                b.set_tooltip_text(Some(i18n("Refresh or long-press for Status Bar").as_str()));
                b.add_css_class("flat");
                b.set_valign(gtk::Align::Center);
                b
            },
            sidebar_refresh_stack: {
                let s = gtk::Stack::new();
                s.set_transition_type(gtk::StackTransitionType::Crossfade);
                s
            },
            sidebar_refresh_spinner: gtk::Spinner::new(),
            sidebar_collapsed: icon_only,
            sidebar_anim: None,
            auto_rail: false,
            rail_active: rail_now,
            sidebar_peek: false,
            peek_transition: std::rc::Rc::new(std::cell::Cell::new(false)),
            peek_split: None,
            peek_refresh_stack: {
                let s = gtk::Stack::new();
                s.set_transition_type(gtk::StackTransitionType::Crossfade);
                s
            },
            peek_refresh_spinner: gtk::Spinner::new(),
            sidebar_hover_expand: prefs.sidebar_hover_expand,
            remember_sidebar,
            remember_rail,
            unified_expanded,
            filtered_expanded,
            tags_expanded,
            starred_expanded,
            sent_expanded,
            drafts_expanded,
            archive_expanded,
            junk_expanded,
            trash_expanded,
            snoozed_expanded,
            favorites,
            favorites_expanded,
            smart_expanded,
            contacts_expanded,
            contacts_folded,
            filtered_expanded_accounts,
            tags_expanded_accounts,
            rail_dots: prefs.rail_dots,
            seen_counts: prefs.seen_counts,
            unread_seen: std::cell::RefCell::new(config::load_unread_seen()),
            rail_fold: prefs.rail_fold,
            app_theme: prefs.app_theme,
            text_scale: config::load_text_scale(),
            theme: config::load_theme(),
            current: None,
            allowed_senders: prefs.allowed_senders,
            unsubscribed: config::load_unsubscribed(),
            invite_answers: config::load_invite_answers(),
            auto_remote_content: prefs.auto_remote_content,
            remote_ahead: prefs.remote_ahead,
            show_remote_banner: prefs.show_remote_banner,
            show_spoof_banner: prefs.show_spoof_banner,
            receipts: prefs.receipts,
            receipts_answered: config::load_receipts(),
            request_receipts: prefs.request_receipts,
            read_by: config::load_read_by(),
            returned_receipts: prefs.returned_receipts,
            notify_receipts: prefs.notify_receipts,
            receipts_filed: Default::default(),
            blacklist: prefs.blacklist,
            palette_collapse_secs: prefs.palette_collapse_secs,
            card_palette_collapse_secs: prefs.card_palette_collapse_secs,
            gravatar: prefs.gravatar,
            avatars: prefs.avatars,
            own_mailbox_face: prefs.own_mailbox_face,
            tag_row_tint: prefs.tag_row_tint,
            contact_book: prefs.contact_book.clone(),
            sender_logos: prefs.sender_logos,
            date_style: config::load_date_format().0,
            clock_style: config::load_date_format().1,
            fetch_interval_secs: prefs.fetch_interval_secs,
            push: prefs.push,
            metered_save_data: prefs.metered_save_data,
            notifications_enabled: prefs.notifications,
            notification_content: prefs.notification_content,
            notification_buttons: config::load_notification_buttons(),
            show_attachments,
            settings_open_accounts: prefs.settings_open_accounts,
            last_settings_page: None,
            list_count: String::new(),
            preview_lines: config::load_preview_lines(),
            list_layout: config::load_list_layout(),
            list_columns: config::load_list_columns(),
            list_headings: prefs.list_headings,
            list_column_scroll: prefs.list_column_scroll,
            list_group_by_date: prefs.list_group_by_date,
            list_pills_below: prefs.list_pills_below,
            list_search_open: prefs.list_search_open,
            contacts_search_open: prefs.contacts_search_open,
            contact_names: prefs.contact_names,
            list_search_bottom: prefs.list_search_bottom,
            list_bulk_bar_bottom: prefs.list_bulk_bar_bottom,
            toolbar_fold_right: prefs.toolbar_fold_right,
            list_column_widths: config::load_list_column_widths(),
            list_wide: config::load_list_pane_width() >= LIST_WIDE_ON,
            shortcuts_win: None,
            run_in_background: std::rc::Rc::new(std::cell::Cell::new(prefs.run_in_background)),
            autostart: config::load_autostart(),
            start_fullscreen: prefs.start_fullscreen,
            tray_enabled: prefs.tray,
            tray_icon: prefs.tray_icon,
            tray_mail: prefs.tray_mail,
            launcher_badge,
            arrivals: Default::default(),
            app_icon: crate::desktop::app_icon::init_on_startup(),
            restart_pending: false,
            tray: None,
            tray_mail_key: std::cell::RefCell::new(None),
            single_key: std::rc::Rc::new(std::cell::Cell::new(prefs.single_key_shortcuts)),
            gmail_keys: std::rc::Rc::new(std::cell::Cell::new(prefs.gmail_keys)),
            keep_downloads_days: prefs.keep_downloads_days,
            threading: prefs.threading,
            thread_render_queued: false,
            thread_opened_at: None,
            thread_related_pending: false,
            thread_painted: false,
            thread_cache: HashMap::new(),
            deleting_attachments: Vec::new(),
            showcase_confirm_delete: false,
            thread_cache_order: Vec::new(),
            thread_key: None,
            threads_expanded: prefs.threads_expanded,
            thread_newest_first: prefs.thread_newest_first,
            fold_messages: prefs.fold_messages,
            always_show_recipients: prefs.always_show_recipients,
            pgp_labels: prefs.pgp_labels,
            reader_find_bottom: prefs.reader_find_bottom,
            show_unified_pref: prefs.show_unified,
            unified_chips: config::load_unified_chips(),
            section_style: prefs.section_style,
            filtered_placement: prefs.filtered_placement,
            tags_placement: prefs.tags_placement,
            favorites_placement: prefs.favorites_placement,
            contacts_placement: prefs.contacts_placement,
            unified_kinds: prefs.unified_kinds,
            show_accounts,
            show_accounts_action,
            focus,
            focus_action,
            list_header_widgets: std::cell::OnceCell::new(),
            chevrons_left: prefs.chevrons_left,
            account_click_folds: prefs.account_click_folds,
            start_view,
            folder_sort: prefs.folder_sort,
            console_mode: prefs.console_mode,
            status_bar_top: prefs.status_bar_top,
            read_mark: prefs.read_mark,
            // The demo (no accounts of its own) ships with tags and filter
            // rules, so its sidebar shows the Tags and Filtered Folders
            // sections; a staged tags.toml / filters.toml still wins.
            saved_addresses: crate::contacts::SavedAddresses::default(),
            quick_filters: QuickFilters::default(),
            filters: {
                let filters = config::load_filters();
                if filters.is_empty() && demo_data {
                    demo_filters()
                } else {
                    filters
                }
            },
            tags: {
                let tags = config::load_tags();
                if tags.is_empty() && demo_data {
                    demo_tags()
                } else {
                    tags
                }
            },
            tag_view: None,
            tag_view_cache: HashMap::new(),
            tag_view_loading: false,
            tag_view_dirty: false,
            smart_folders: config::load_smart_folders(),
            smart_view: None,
            smart_view_cache: HashMap::new(),
            smart_view_loading: false,
            smart_view_dirty: false,
            unified_starred_loading: false,
            unified_starred_dirty: false,
            unified_starred_rows: HashMap::new(),
            unified_starred_due: false,
            preload_due: false,
            prebuild_queue: Vec::new(),
            prebuild_due: false,
            smart_counts_due: false,
            smart_counts_running: false,
            keyword_sync_at: HashMap::new(),
            flag_sync_at: HashMap::new(),
            tag_provider: crate::ui::DisplayCss::new(),
            // Demo mode gets a cache that never touches disk, seeded with the
            // sample mail: the gallery's paging, search and sort are database
            // queries, so the demo has to run them against a real database or
            // it would be exercising nothing. Not with real accounts: their
            // workers keep the real one, and the lists read it.
            cache: if demo_data {
                let c = crate::cache::Cache::in_memory().ok();
                if let Some(c) = c.as_ref() {
                    crate::backend::seed_demo_cache(c);
                }
                c
            } else {
                crate::cache::Cache::open().ok()
            },
            filter_run: None,
            filter_moved: Default::default(),
            forwarded_log: config::ForwardedLog::load(),
            auto_forwards: Vec::new(),
            auto_forward_inflight: Default::default(),
            auto_forward_tickets: Default::default(),
            filter_followups: Default::default(),
            body_hits: Default::default(),
            single_message_card: prefs.single_message_card,
            reader_mode: prefs.reader_mode,
            zoom: config::load_reader_zoom(),
            zoom_default: config::load_reader_zoom(),
            reader_switch: prefs.reader_switch,
            theme_switch: prefs.theme_switch,
            desktop_icons: prefs.desktop_icons,
            reader_default: prefs.reader_default,
            card_attachments: prefs.card_attachments,
            drawer_enabled: prefs.attachment_drawer,
            thread_expansion: prefs.thread_expansion,
            thread_row_newest: prefs.thread_row_newest,
            confirm_thread_delete: prefs.confirm_thread_delete,
            selection_from_cards: false,
            card_actions_hover: prefs.card_actions_hover,
            card_actions_auto: prefs.card_actions_auto,
            list_palette: prefs.list_palette,
            list_palette_hover: prefs.list_palette_hover,
            card_palette_menu: prefs.card_palette_menu,
            swipe_enabled: prefs.swipe_enabled,
            swipe_reversed: prefs.swipe_reversed,
            swipe_sensitivity: config::load_swipe_sensitivity(),
            compose_inline: prefs.compose_inline,
            reply_inline: prefs.reply_inline,
            reply_fields: prefs.reply_fields,
            compose_default_from: prefs.compose_default_from,
            paste_plain: prefs.paste_plain,
            return_paragraph: prefs.return_paragraph,
            toolbar_expanded: prefs.toolbar_expanded,
            compose_font: prefs.compose_font,
            compose_font_size: prefs.compose_font_size,
            compose_zoom: prefs.compose_zoom,
            print_options: prefs.print,
            compose_format: config::load_compose_format(),
            follow_plain: prefs.follow_plain,
            reply_position: prefs.reply_position,
            signature_position: prefs.signature_position,
            signature_dashes: prefs.signature_dashes,
            reply_header: prefs.reply_header,
            undo_send_secs: prefs.undo_send_secs,
            spellcheck: prefs.spellcheck,
            spellcheck_langs: prefs.spellcheck_langs,
            message_theme: prefs.message_theme,
            override_fonts: reader_override.0,
            reader_font: reader_override.1,
            override_colors: reader_override.2,
            plain_monospace: plain_style.0,
            plain_font: plain_style.1,
            auto_fetch_source: None,
            last_wake: None,
            notifications,
            welcome: None,
            notify_count: 0,
            busy: HashSet::new(),
            sidebar,
            peek_sidebar,
            message_list,
            message_view,
            attachment_drawer,
            gallery,
            showing_gallery: false,
            contacts_page,
            showing_contacts: false,
            showing_outbox: false,
            outbox_by_account: HashMap::new(),
            snoozed: super::snooze::load_snoozed(),
            snoozed_row: std::cell::Cell::new(false),
            snoozed_held: HashMap::new(),
            snooze_waking: HashMap::new(),
            snooze_undone: HashMap::new(),
            snooze_until: None,
            server_identities: HashMap::new(),
            unified_emit_queued: std::cell::Cell::new(false),
            unified_dirty: std::cell::Cell::new(false),
            thread_links_due: std::cell::Cell::new(false),
            unified_emitted_at: std::cell::Cell::new(std::time::Instant::now()),
            list_fp: std::cell::Cell::new(None),
            unified_unfilled: std::collections::HashSet::new(),
            gallery_total: 0,
            gallery_scan_left: HashMap::new(),
        }
    }
}
