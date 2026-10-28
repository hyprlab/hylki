//! Root component: the application window, three-pane adaptive layout, and the
//! routing between the sidebar, list, reader, and the per-account mail workers.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n;
use crate::ui::notifications::NotifyInput;
use crate::ui::FOCUS_ANIM_MS;

// The message list's opening width now comes from config (the remembered pane
// width, #28); its floor lives with the pane in message_list/layout.rs
// (LIST_MIN_WIDTH).

relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AccountsAction, WindowActionGroup, "accounts");
relm4::new_stateless_action!(PreferencesAction, WindowActionGroup, "preferences");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(ShortcutsAction, WindowActionGroup, "shortcuts");
relm4::new_stateless_action!(PrintAction, WindowActionGroup, "print");
relm4::new_stateless_action!(PrintPreviewAction, WindowActionGroup, "print-preview");
/// Store a setting the user changed and save the settings, if it really
/// changed; says whether it did, so the caller can apply it.
macro_rules! pref {
    ($self:ident . $($field:ident).+ = $value:expr) => {{
        let value = $value;
        let changed = $self.$($field).+ != value;
        if changed {
            $self.$($field).+ = value;
            $self.save_settings();
        }
        changed
    }};
}

mod about;
mod accounts;
mod actions;
mod attachments;
mod boot;
mod card_menu;
mod children;
mod compose_host;
mod compose_init;
mod compose_open;
mod consts;
mod contacts;
mod conversation;
mod counts;
mod demo;
mod diagnostics;
mod drafts;
mod events_bodies;
mod events_folders;
mod events_messages;
mod filter_actions;
mod filter_forward;
mod filter_run;
mod filters;
mod flags;
mod focus;
mod folder_menu;
mod folders;
mod gallery;
mod hand_off;
mod identity;
mod invites;
mod keymap;
mod list_header;
mod model;
mod model_new;
mod move_tag;
mod msg;
mod navigation;
mod notifications;
mod outbox;
mod outgoing;
mod palette;
mod pending;
mod picker;
mod popout;
mod prefill;
mod prefs_compose;
mod prefs_data;
mod prefs_general;
mod prefs_list;
mod prefs_reader;
mod prefs_sidebar;
mod reader_select;
mod reader_toolbar;
mod receipts;
mod receipts_returned;
mod release_notes;
mod removal;
mod router;
mod search;
mod selection;
mod send;
mod send_morph;
mod settings_store;
mod settings_window;
mod shortcuts;
mod showcase;
mod sidebar;
mod sidebar_contents;
mod sidebar_peek;
mod smart;
mod snooze;
mod split_reply;
mod tags;
#[cfg(test)]
mod test_support;
mod text_search;
mod theme;
mod transfer;
mod tray;
mod types;
mod undo;
mod unified;
mod unified_starred;
mod unsubscribe;
mod uri_queue;
mod view_flags;
mod window;
mod workers;

pub(crate) use accounts::finish_keyring_writes;
pub(crate) use model::AppModel;
pub(crate) use msg::AppMsg;
pub(crate) use uri_queue::{
    is_message_file, queue_attach_files, queue_mailto, queue_message_file, queue_mid,
};

use attachments::schedule_download_prune;
use boot::{build_main_menu, Boot};
use children::Children;
use consts::READER_MIN_WIDTH;
use contacts::start_contacts_sync;
use diagnostics::start_heap_trim;
use palette::palette_key;
use theme::{apply_app_theme, follow_motion_setting};
use window::{exit_now, install_window_actions, save_window_geometry};
use workers::watch_system_events;

relm4::new_stateless_action!(StatusBarAction, WindowActionGroup, "status-bar");
relm4::new_stateless_action!(ConsoleAction, WindowActionGroup, "console");
relm4::new_stateless_action!(FindAction, WindowActionGroup, "find");
relm4::new_stateless_action!(UndoAction, WindowActionGroup, "undo");
relm4::new_stateless_action!(RedoAction, WindowActionGroup, "redo");
relm4::new_stateful_action!(FullscreenAction, WindowActionGroup, "fullscreen", (), bool);

#[relm4::component(pub(crate))]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some(crate::APP_NAME),
            set_icon_name: Some(crate::APP_ID),
            add_css_class: "hylki",

            // Persist the window size + maximized state on close. (Position and
            // which monitor can't be restored on Wayland — the compositor owns
            // placement — so only the geometry is saved.)
            connect_close_request[background = model.run_in_background.clone(), outgoing = model.outgoing.clone()] => move |w| {
                save_window_geometry(w);
                // Running in the background: hide the window and stay alive, so
                // mail keeps arriving and GNOME lists Hylki under Background Apps
                // (issue #3). The window is kept rather than rebuilt, so reopening
                // is instant and nothing is torn down — which is also what makes
                // this safe, given the exit below.
                if background.get() {
                    w.set_visible(false);
                    return gtk::glib::Propagation::Stop;
                }
                exit_now(&outgoing)
            },

            // Send's toasts (#434) float over everything, the status bar
            // included, and never move the layout.
            #[wrap(Some)]
            #[name = "toasts"]
            set_content = &adw::ToastOverlay {
            #[wrap(Some)]
            #[name = "window_overlay"]
            set_child = &gtk::Overlay {

                #[wrap(Some)]
                set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                // The peek panel's split view: permanently collapsed, so
                // showing its sidebar slides the expanded panel in from the
                // window's left edge over the rail and the panes — with
                // libadwaita's scrim, shadow and swipe — while the docked
                // rail underneath stays exactly as it is (see
                // set_sidebar_peek). The account split view is its content.
                #[name = "peek_split"]
                adw::OverlaySplitView {
                    set_vexpand: true,
                    set_collapsed: true,
                    set_show_sidebar: false,
                    set_min_sidebar_width: 280.0,
                    set_max_sidebar_width: 280.0,
                    // No edge-swipe to open: it would fight the message
                    // list's own swipe actions.
                    set_enable_show_gesture: false,

                    #[wrap(Some)]
                    set_sidebar = &adw::ToolbarView {
                        // Laid out like the expanded sidebar's header: Refresh
                        // top-left, the title centred, the menu top-right.
                        add_top_bar = &adw::HeaderBar {
                            add_css_class: "flat",
                            set_show_start_title_buttons: false,
                            set_show_end_title_buttons: false,
                            #[wrap(Some)]
                            set_title_widget = &gtk::Label {
                                set_label: crate::APP_NAME,
                                add_css_class: "app-title",
                            },
                            pack_start = &gtk::Button {
                                set_tooltip_text: Some(i18n("Refresh or long-press for Status Bar").as_str()),
                                add_css_class: "flat",
                                set_valign: gtk::Align::Center,
                                set_child: Some(&model.peek_refresh_stack),
                                connect_clicked[sender] => move |_| sender.input(AppMsg::Refresh),
                                add_controller = gtk::GestureLongPress {
                                    connect_pressed[sender] => move |gesture, _, _| {
                                        gesture.set_state(gtk::EventSequenceState::Claimed);
                                        sender.input(AppMsg::ToggleNotifications);
                                    },
                                },
                            },
                            pack_end = &gtk::MenuButton {
                                // Hylki's own copy of the three bars: an icon of
                                // the GNOME name from a theme or another program
                                // drew a gear on some systems.
                                set_icon_name: "hylki-open-menu-symbolic",
                                set_tooltip_text: Some(i18n("Main Menu").as_str()),
                                add_css_class: "flat",
                                set_menu_model: Some(&model.menu),
                            },
                        },
                        #[wrap(Some)]
                        set_content = model.peek_sidebar.widget(),
                    },

                    #[wrap(Some)]
                    #[name = "sidebar_split"]
                    set_content = &adw::OverlaySplitView {
                    set_vexpand: true,
                    set_max_sidebar_width: 280.0,
                    // Low enough (with the panes' own minimums) that the whole
                    // window fits half of a 1920px screen — see READER_MIN_WIDTH.
                    set_min_sidebar_width: 180.0,
                    set_sidebar_width_fraction: 0.2,

                    #[wrap(Some)]
                    set_sidebar = &adw::ToolbarView {
                        #[name = "sidebar_header"]
                        add_top_bar = &adw::HeaderBar {
                            add_css_class: "flat",
                            #[wrap(Some)]
                            #[name = "app_title"]
                            set_title_widget = &gtk::Label {
                                set_label: crate::APP_NAME,
                                add_css_class: "app-title",
                            },
                            pack_start: &model.sidebar_refresh,
                            #[name = "sidebar_menu"]
                            pack_end = &gtk::MenuButton {
                                set_icon_name: "hylki-open-menu-symbolic",
                                set_tooltip_text: Some(i18n("Main Menu").as_str()),
                                add_css_class: "flat",
                                set_menu_model: Some(&model.menu),
                            },
                        },
                        #[wrap(Some)]
                        set_content = model.sidebar.widget(),
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,

                    #[name = "content_stack"]
                    gtk::Stack {
                        set_hexpand: true,
                        // Swap the mail panes for the attachments gallery or
                        // the contacts view, at once: no transition.
                        #[watch]
                        set_visible_child_name: if model.showing_gallery {
                            "gallery"
                        } else if model.showing_contacts {
                            "contacts"
                        } else {
                            "mail"
                        },

                    add_named[Some("mail")] = &gtk::Paned {
                        set_orientation: gtk::Orientation::Horizontal,
                        // Thin handle so the panes sit flush (just a 1px divider),
                        // no wide-handle gap between them. A theme paints that
                        // divider through this class (see theme::css); without
                        // one it keeps the stock separator.
                        add_css_class: "mail-split",
                        set_wide_handle: false,
                        // Launch wide enough for a row's actions palette. That is
                        // also the list's minimum while the avatars are on,
                        // so `shrink_start_child: false` clamps to the same figure
                        // either way. With the circles off the minimum drops to what
                        // the sender and subject need (#29) — a fine width to be
                        // able to drag down to, but a poor one to open at.
                        set_position: crate::config::load_list_pane_width(),
                        // Remember the width the user drags to (#28). Debounced:
                        // position-notify fires per pixel of a drag (and when the
                        // window squeezes the pane), one write once it settles.
                        connect_position_notify[
                            sender,
                            pending = std::rc::Rc::new(std::cell::RefCell::new(
                                None::<gtk::glib::SourceId>,
                            ))
                        ] => move |p| {
                            let pos = p.position();
                            // Automatic layout follows the width as it is
                            // dragged, not once the drag has settled (#334).
                            sender.input(AppMsg::ListPaneWidth(pos));
                            if let Some(id) = pending.borrow_mut().take() {
                                id.remove();
                            }
                            let armed = pending.clone();
                            *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(
                                std::time::Duration::from_millis(600),
                                move || {
                                    *armed.borrow_mut() = None;
                                    crate::config::save_list_pane_width(pos);
                                },
                            ));
                        },
                        // The list keeps its width as the window resizes (the
                        // reader absorbs the change), and can't be dragged narrower
                        // than its own minimum. The reader has a floor of its own:
                        // its header carries a full row of actions, and squeezing it
                        // pushed them off the right-hand edge of the window.
                        set_resize_start_child: false,
                        set_shrink_start_child: false,
                        set_resize_end_child: true,
                        set_shrink_end_child: false,

                        #[wrap(Some)]
                        set_start_child = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                add_css_class: "flat",
                                // Middle pane: no window controls (the reader pane's
                                // header carries the window's close button).
                                set_show_start_title_buttons: false,
                                set_show_end_title_buttons: false,
                                // No folder title here — the sidebar's selection
                                // already names it. An empty label keeps the
                                // window's "Hylki" title from appearing instead.
                                #[wrap(Some)]
                                set_title_widget = &gtk::Label {
                                    set_label: "",
                                },
                                // Leftmost, mirroring the pane it acts on: the
                                // sidebar expand/collapse toggle (moved here from
                                // the sidebar's own footer).
                                pack_start = &gtk::Button {
                                    set_icon_name: "sidebar-show-symbolic",
                                    #[watch]
                                    set_tooltip_text: Some(if model.rail_active { i18n("Expand sidebar") } else { i18n("Collapse sidebar") }.as_str()),
                                    add_css_class: "flat",
                                    connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleSidebar),
                                },
                                // Search lives behind this button (#102);
                                // Ctrl+F and / open it too.
                                #[name = "lh_search"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "system-search-symbolic",
                                        set_tooltip_text: Some(i18n("Search messages (Ctrl+F)").as_str()),
                                        add_css_class: "flat",
                                        connect_clicked[sender] => move |_| {
                                            sender.input(AppMsg::OpenListSearch);
                                        },
                                    },
                                },
                                // Across from the sidebar toggle: the visible
                                // message count and the sort menu (moved out of
                                // the list's own toolbar to reclaim a row).
                                // pack_end packs right-to-left: sort rightmost.
                                // Quick filters (#97, #441): session state,
                                // like Mail.app's filter bar.
                                // Focus Mode: the ⋯ that stands in for the
                                // search, filters, count and sort while they
                                // are folded away. Rightmost, so packed first.
                                #[name = "lh_overflow"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    set_reveal_child: false,
                                    gtk::Button {
                                        set_icon_name: "view-more-horizontal-symbolic",
                                        set_tooltip_text: Some(i18n("Search, filters and sort").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ListOverflowMenu),
                                    },
                                },
                                // The sort orders and every quick filter share
                                // one popover (#441): a button each would crowd
                                // the header.
                                #[name = "lh_filter"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    #[name = "list_filter_btn"]
                                    gtk::MenuButton {
                                        set_icon_name: "filter-symbolic",
                                        set_tooltip_text: Some(i18n("Sort and filter messages").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                    },
                                },

                                #[name = "lh_count"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Label {
                                        #[watch]
                                        set_label: &model.list_count,
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "list-count",
                                    },
                                },
                            },
                            #[wrap(Some)]
                            set_content = model.message_list.widget(),
                        },

                        #[wrap(Some)]
                        #[name = "reader_bin"]
                        set_end_child = &adw::BreakpointBin {
                            // The narrowest the reader may become. The header's
                            // actions collapse into the overflow menu before this
                            // matters (see the breakpoint added in init). The
                            // height floor exists because AdwBreakpointBin insists
                            // on an explicit one; the window's own minimum is what
                            // really binds vertically.
                            set_size_request: (READER_MIN_WIDTH, 200),
                            #[wrap(Some)]
                            set_child = &adw::ToolbarView {
                            #[name = "reader_header"]
                            add_top_bar = &adw::HeaderBar {
                                // Rightmost of the packed items, beside the window
                                // controls: the overflow ⋯ that stands in for the
                                // action buttons while collapsed.
                                pack_end: &model.reader_overflow_btn,
                                add_css_class: "flat",
                                // Tighter icon spacing than stock so the full
                                // action row fits a narrower pane (see
                                // READER_ACTIONS_BREAKPOINT and styles.css).
                                add_css_class: "reader-toolbar",
                                // Empty title so the window's "Hylki" title isn't
                                // shown here; the app title lives above the sidebar.
                                #[wrap(Some)]
                                set_title_widget = &gtk::Label {
                                    set_label: "",
                                },
                                // Outbox actions, in place of Reply/Forward/Flag —
                                // a message that hasn't been sent can't be replied
                                // to, and the questions worth asking about it are
                                // whether to edit, send or bin it.
                                #[name = "tb_outbox_edit"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "document-edit-symbolic",
                                        set_tooltip_text: Some(i18n("Edit this message").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::EditCurrentOutbox),
                                    },
                                },
                                #[name = "tb_outbox_send"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-send-symbolic",
                                        set_tooltip_text: Some(i18n("Try to send this message now").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::SendCurrentOutbox),
                                    },
                                },
                                #[name = "tb_outbox_send_all"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_label: &i18n("Send all"),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::RetryAllOutbox),
                                    },
                                },
                                // ---- The action buttons. Declared here in the
                                // default order, then re-packed in init (and on
                                // every change) in the user's saved order — see
                                // relayout_reader_toolbar. The left group stays
                                // at every width; the right group folds into
                                // the ⋯ overflow. Default left group: Reply,
                                // Reply All, Forward, Star, Archive, Spam,
                                // Delete.
                                #[name = "tb_reply"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-reply-sender-symbolic",
                                        set_tooltip_text: Some(i18n("Reply").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Reply),
                                        // In a conversation these act on the one
                                        // highlighted card; with none (or several)
                                        // highlighted they grey out — no way to say
                                        // which message they'd mean.
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Reply),
                                    },
                                },
                                #[name = "tb_reply_all"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-reply-all-symbolic",
                                        set_tooltip_text: Some(i18n("Reply All").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::ReplyAll),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ReplyAll),
                                    },
                                },
                                #[name = "tb_forward"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-forward-symbolic",
                                        set_tooltip_text: Some(i18n("Forward").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Forward),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Forward),
                                    },
                                },
                                #[name = "tb_star"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        #[watch]
                                        set_tooltip_text: Some(if model.toolbar_star_lit() { i18n("Remove Star") } else { i18n("Star") }.as_str()),
                                        // One glyph in both states, like every other
                                        // icon; the starred state carries color only.
                                        set_icon_name: "hylki-non-starred-symbolic",
                                        #[watch]
                                        set_css_classes: if model.toolbar_star_lit() {
                                            &["flat", "star-active"]
                                        } else {
                                            &["flat"]
                                        },
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Star),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleStar),
                                    },
                                },
                                #[name = "tb_archive"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-archive-symbolic",
                                        set_tooltip_text: Some(i18n("Archive").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Archive),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Archive),
                                    },
                                },
                                #[name = "tb_delete"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "user-trash-symbolic",
                                        #[watch]
                                        set_tooltip_text: Some(&model.delete_tooltip()),
                                        add_css_class: "flat",
                                        // Shown for the Outbox too (a queued message
                                        // can still be binned) — see toolbar_visible.
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Delete),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some()
                                            || model.many_selected(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Delete),
                                    },
                                },
                                // ---- Default right group, left to right: Tags,
                                // Read/Unread, Spam, Move To, Find, Print.
                                // (The sender-check seal lives in the
                                // message header now — #88; no Add-to-Contacts
                                // button either: right-click any address in a
                                // message header.)
                                #[name = "tb_print"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "printer-symbolic",
                                        set_tooltip_text: Some(i18n("Print Preview (Ctrl+Shift+P)").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Print),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        // The preview, not the print dialog: the button
                                        // shows what will come out and prints from
                                        // there, so nobody spends paper to find out.
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::PrintPreview),
                                    },
                                },
                                // In-message find (#103).
                                #[name = "tb_find"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "loupe-with-arrow-symbolic",
                                        set_tooltip_text: Some(i18n("Find in message (Ctrl+F)").as_str()),
                                        add_css_class: "flat",
                                        // Greyed out, not hidden, with no message
                                        // open: the toolbar must not shift.
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Find),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| {
                                            sender.input(AppMsg::OpenReaderFind);
                                        },
                                    },
                                },
                                // Move To… (#164): a folder picker for the
                                // target, or the whole list selection.
                                #[name = "tb_move"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Box {
                                        #[local_ref]
                                        reader_move_btn -> gtk::Button {
                                            #[watch]
                                            set_visible: model.toolbar_visible(config::ToolbarItem::MoveTo),
                                            #[watch]
                                            set_sensitive: model.reply_target().is_some()
                                                || model.many_selected(),
                                        },
                                    },
                                },
                                // Snooze (#527): a menu of times, greyed out
                                // where nothing can be snoozed (Drafts, POP3).
                                #[name = "tb_snooze"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Box {
                                        #[local_ref]
                                        reader_snooze_btn -> gtk::Button {
                                            #[watch]
                                            set_visible: model.toolbar_visible(config::ToolbarItem::Snooze),
                                            #[watch]
                                            set_sensitive: model.snooze_offered(),
                                        },
                                    },
                                },
                                #[name = "tb_spam"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        #[watch]
                                        set_icon_name: if model.target_in_junk() {
                                            "mail-mark-notjunk-symbolic"
                                        } else {
                                            "mail-mark-junk-symbolic"
                                        },
                                        #[watch]
                                        set_tooltip_text: Some(if model.target_in_junk() { i18n("Not Spam") } else { i18n("Mark as Spam") }.as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Spam),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::MarkSpam),
                                    },
                                },
                                #[name = "tb_read"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::ReadUnread),
                                        // The icon shows the ACTION (read envelope =
                                        // "mark as read"), matching the menus.
                                        #[watch]
                                        set_icon_name: if model.reply_target().is_some_and(|m| m.unread) {
                                            "hylki-mail-read-symbolic"
                                        } else {
                                            "mail-unread-symbolic"
                                        },
                                        #[watch]
                                        set_tooltip_text: Some(if model.reply_target().is_some_and(|m| m.unread) { i18n("Mark as Read") } else { i18n("Mark as Unread") }.as_str()),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleReadCurrent),
                                    },
                                },
                                // Tags (#71): a menu of the tags, ticked where
                                // the target carries them. Only once a tag exists.
                                #[name = "tb_tags"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Box {
                                        #[local_ref]
                                        reader_tag_btn -> gtk::Button {
                                            #[watch]
                                            set_visible: model.toolbar_visible(config::ToolbarItem::Tags) && !model.tags.is_empty(),
                                            #[watch]
                                            set_sensitive: model.acts_on_selection(),
                                        },
                                    },
                                },
                                #[name = "tb_spinner"]
                                pack_end = &gtk::Spinner {
                                    set_valign: gtk::Align::Center,
                                    set_tooltip_text: Some(i18n("Downloading attachments…").as_str()),
                                    #[watch]
                                    set_spinning: model.attachments_loading,
                                    #[watch]
                                    set_visible: model.attachments_loading
                                        && model.reader_compose.is_none(),
                                },
                            },
                            // Reader content: the inline reply/forward pane drops
                            // down (SlideDown revealer) above the message body,
                            // pushing it down to make room. The revealer is
                            // prepended in `init`. The drawer's widget is a Paned
                            // that holds the reader body + attachment footer.
                            #[wrap(Some)]
                            #[name = "reader_content_box"]
                            set_content = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                append: model.attachment_drawer.widget(),
                            },
                            },
                        },
                    },
                    },
                    },
                    },
                },

                // The status bar sits at the foot of the window (#433).
                append: model.notifications.widget(),
                },

                // ======== Full-window attachment lightbox ========
                // Covers all three panes; shown for images and PDFs coming
                // from the drawer. Same look as the gallery's own lightbox
                // (same CSS), no extra window.
                add_overlay: model.lightbox.widget(),
            },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let boot = Boot::read(&root);
        let children = Children::launch(&boot, &sender);
        let main_menu = build_main_menu(boot.prefs.show_accounts, &boot.focus, &sender);
        let mut model = AppModel::new(&root, &sender, boot, children, main_menu);
        model.prime_and_connect(&sender);
        schedule_download_prune();
        text_search::prepare_text_search();
        model.withdraw_stale_notifications();
        if model.tray_enabled {
            model.start_tray(&sender);
        }
        start_contacts_sync(&sender);
        if model.contact_names {
            model.read_contact_names(&sender);
        }
        watch_system_events(&sender);
        model.open_wizard_if_needed(&sender);
        model.push_settings_to_children();
        model.arm_auto_fetch(&sender);

        // The app-wide theme choice must be in force before the first frame.
        apply_app_theme(model.app_theme);
        crate::desktop::text_scale::apply(model.text_scale);
        follow_motion_setting();
        let reader_tag_btn = model.reader_tag_btn.clone();
        let reader_move_btn = model.reader_move_btn.clone();
        let reader_snooze_btn = model.reader_snooze_btn.clone();
        let widgets = view_output!();
        let _ = model.reader_header.set(widgets.reader_header.clone());
        let _ = model.toasts.set(widgets.toasts.clone());
        model.init_reader_header_menu(&widgets, &sender);
        model.init_reader_toolbar_fold(&widgets, &sender);
        model.init_compose_overlay(&widgets);
        model.init_gallery_page(&widgets, &sender);
        model.install_notification_actions(&sender);
        model.init_window_size(&root, &widgets, &sender);
        model.init_hover_peek(&widgets, &sender);
        model.sidebar_split = Some(widgets.sidebar_split.clone());
        model.peek_split = Some(widgets.peek_split.clone());
        model.app_title = Some(widgets.app_title.clone());
        model.sidebar_header = Some(widgets.sidebar_header.clone());
        model.sidebar_menu = Some(widgets.sidebar_menu.clone());
        model.init_refresh_buttons(&widgets, &sender);

        model.install_app_actions(&root, &sender);

        let (undo_action, redo_action) = install_window_actions(&root, &sender);

        root.add_controller(palette_key(&sender));

        model.install_key_controller(&root, &sender);

        model.show_startup_notices(&sender);

        start_heap_trim();
        model.install_showcase_probes(&sender);
        model.stage_showcase(&root, &widgets, &sender);

        model.init_list_sort_menu(&widgets, &sender);

        model.rebuild_help_menu();
        model.undo_action = Some(undo_action);
        model.redo_action = Some(redo_action);
        // Both start empty, so both start greyed out.
        model.refresh_undo_menu();
        model
            .notifications
            .emit(NotifyInput::SetConsoleEnabled(model.console_mode));
        model
            .notifications
            .emit(NotifyInput::SetAtTop(model.status_bar_top));
        model.install_showcase_overlays(&sender);
        model.drain_hand_off_queues(&sender);

        model.install_drop_targets(&root, &widgets, &sender);

        model.start_outbox_clocks(&sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        self.route(msg, sender)
    }
}
