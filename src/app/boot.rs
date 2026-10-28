//! The startup steps that run before the root component's model exists:
//! process-wide state (network, CSS, icons, theme, web fonts), the sidebar
//! layout and Focus Mode, the accounts reconciled against GNOME Online
//! Accounts, and the main menu and its two stateful actions.

use adw::prelude::*;
use relm4::ComponentSender;

use crate::config::{self, AccountConfig};
use crate::i18n::i18n;
use crate::ui::preferences::PrefOutput;
use crate::ui::sidebar::{SidebarInit, StartTarget};

use super::accounts::{forget_account_secrets, reconcile_goa, GoaReconcile};
use super::demo::demo_mode;
use super::palette::keyed_item;
use super::theme::install_scheme_css;
use super::{AppModel, AppMsg};
use crate::desktop::icon_theme::register_icons;

/// What the steps before the model read from disk and the environment.
pub(super) struct Boot {
    /// Every setting in privacy.toml, read once.
    pub(super) prefs: config::PrivacyFile,
    /// The sidebar layout, already pruned of accounts GOA removed and reset
    /// when "Remember the sidebar layout" is off.
    pub(super) sidebar_state: config::SidebarState,
    pub(super) focus: config::FocusMode,
    /// The user's own icon-rail choice, the one that is saved.
    pub(super) icon_only: bool,
    /// Whether the sidebar starts as the rail: the user's choice or Focus
    /// Mode's rail part.
    pub(super) rail_now: bool,
    pub(super) config: Vec<AccountConfig>,
    /// Whether this run serves the built-in sample data (see spawn_workers).
    pub(super) demo_data: bool,
}

impl Boot {
    /// Runs the steps in the order they depend on each other.
    pub(super) fn read(root: &adw::ApplicationWindow) -> Self {
        let prefs = install_globals(root);
        let (mut sidebar_state, focus, icon_only, rail_now) = load_sidebar_layout(&prefs);
        let config = load_accounts(&mut sidebar_state);
        // Decided after the GOA reconcile, which can add accounts.
        let demo_data = demo_mode() && config.is_empty();
        Boot {
            prefs,
            sidebar_state,
            focus,
            icon_only,
            rail_now,
            config,
            demo_data,
        }
    }
}

/// Reads privacy.toml and puts the process-wide state in place.
fn install_globals(root: &adw::ApplicationWindow) -> config::PrivacyFile {
    let prefs = config::load_privacy();
    // Before the workers start, so their first connection already knows
    // whether there is a network to make it on (#442).
    crate::desktop::power::read_network(prefs.metered_save_data);
    relm4::set_global_css(include_str!("../styles.css"));
    register_icons(prefs.desktop_icons);
    // Before install_scheme_css and before the reader exists: both read
    // the theme's colors back, and both listen for the light/dark flip
    // that swaps a theme's two palettes — GTK runs those handlers in
    // connection order, so the palette has to be in place first.
    crate::desktop::theme::install(&config::load_theme());
    install_scheme_css(root);
    // Before any web view loads: a font fallback WebKit needed under
    // this runtime has to be in place for its first web process (#296).
    crate::desktop::web_fonts::startup(root);
    prefs
}

/// The saved sidebar layout and Focus Mode, as this launch starts them:
/// `(layout, focus, icon_only, rail_now)`.
fn load_sidebar_layout(
    prefs: &config::PrivacyFile,
) -> (config::SidebarState, config::FocusMode, bool, bool) {
    let mut sidebar_state = config::load_sidebar_state();
    // Settings → Sidebar: "Remember the icon rail" off starts every
    // launch with the full sidebar; "Remember the sidebar layout" off
    // starts with every section folded up — and every account, folded as
    // it arrives (SetAccount) — keeping only the account order. The two
    // are independent.
    let icon_only = prefs.remember_rail && sidebar_state.icon_only;
    // Read here, ahead of the sidebar itself: Focus Mode's rail part
    // decides what the sidebar is built as. `icon_only` stays the user's
    // own choice — the one that is saved and the one that comes back
    // when Focus Mode ends.
    let mut focus = config::load_focus_mode();
    // "Start in Focus Mode" decides how the app opens, whatever the last
    // session left on. Written back at once so the file, the menu's check
    // item and the window all say the same thing from the first frame.
    if focus.enabled != focus.start_focused {
        focus.enabled = focus.start_focused;
        config::save_focus_mode(&focus);
    }
    let rail_now = icon_only || focus.active(config::FocusPart::RailSidebar);
    if !prefs.remember_sidebar {
        // The layout starts fresh; the order things were put in stays.
        sidebar_state = config::SidebarState {
            order: sidebar_state.order,
            folder_order: sidebar_state.folder_order,
            ..Default::default()
        };
    }
    (sidebar_state, focus, icon_only, rail_now)
}

/// Load accounts, then reconcile against GNOME Online Accounts: drop any
/// imported account GOA no longer has, pause any whose Mail service is
/// switched off there. Reconciliation is skipped when GOA is unreachable,
/// so a momentary outage never wipes imported accounts. Live changes are
/// handled by the GOA watcher.
fn load_accounts(sidebar_state: &mut config::SidebarState) -> Vec<AccountConfig> {
    let mut config = config::load().unwrap_or_default();
    // Snapshot before reconciling: a removed account's alias-password
    // keyring entries are keyed by addresses only its config knows.
    let config_before_goa = config.clone();
    let goa_outcome = match crate::services::goa::live_state() {
        Some(live) => reconcile_goa(&mut config, &live),
        None => GoaReconcile::default(),
    };
    let goa_removed = goa_outcome.removed;
    if !goa_removed.is_empty() {
        forget_account_secrets(&goa_removed, &config_before_goa);
        sidebar_state.order.retain(|e| !goa_removed.contains(e));
        sidebar_state.collapsed.retain(|e| !goa_removed.contains(e));
        sidebar_state
            .folders_expanded
            .retain(|e| !goa_removed.contains(e));
        sidebar_state
            .filtered_expanded_accounts
            .retain(|e| !goa_removed.contains(e));
        sidebar_state
            .tags_expanded_accounts
            .retain(|e| !goa_removed.contains(e));
        config::save_sidebar_state(sidebar_state);
    }
    if !goa_removed.is_empty() || goa_outcome.paused_changed || goa_outcome.servers_changed {
        let _ = config::save(&config);
    }
    config
}

/// The view the sidebar picks first instead of All Inboxes (#256), from
/// Settings → General's start view and the last view left.
pub(super) fn start_target(start_view: config::StartView) -> Option<StartTarget> {
    let (last, last_account) = config::load_last_view();
    match start_view {
        config::StartView::AllInboxes => None,
        config::StartView::AccountInbox => Some(last_account)
            .filter(|e| !e.is_empty())
            .map(StartTarget::Inbox),
        config::StartView::LastFolder => last
            .filter(|v| !v.email.is_empty())
            .map(|v| StartTarget::Folder(v.email, v.path)),
    }
}

/// The launch settings of a sidebar instance: the sections open as the
/// saved layout left them. The main sidebar and the peek panel's mirror
/// differ only in `collapsed`, `mirror` and `start`.
pub(super) fn sidebar_init(
    state: &config::SidebarState,
    show_attachments: bool,
    collapsed: bool,
    mirror: bool,
    start: Option<StartTarget>,
) -> SidebarInit {
    SidebarInit {
        collapsed,
        mirror,
        unified_expanded: state.unified_expanded,
        filtered_expanded: state.filtered_expanded,
        tags_expanded: state.tags_expanded,
        starred_expanded: state.starred_expanded,
        sent_expanded: state.sent_expanded,
        drafts_expanded: state.drafts_expanded,
        archive_expanded: state.archive_expanded,
        junk_expanded: state.junk_expanded,
        trash_expanded: state.trash_expanded,
        snoozed_expanded: state.snoozed_expanded,
        favorites_expanded: state.favorites_expanded,
        smart_expanded: state.smart_expanded,
        contacts_expanded: state.contacts_expanded,
        contacts_folded: state.contacts_folded.clone(),
        show_attachments,
        start,
    }
}

/// The primary menu, the two menus filled in later (help by
/// rebuild_help_menu, undo/redo by refresh_undo_menu) and the actions of
/// its two check items.
pub(super) struct MainMenu {
    pub(super) menu: gtk::gio::Menu,
    pub(super) help: gtk::gio::Menu,
    pub(super) undo: gtk::gio::Menu,
    pub(super) show_accounts_action: gtk::gio::SimpleAction,
    pub(super) focus_action: gtk::gio::SimpleAction,
}

/// Sectioned: settings / printing / window & help / quit.
pub(super) fn build_main_menu(
    show_accounts: bool,
    focus: &config::FocusMode,
    sender: &ComponentSender<AppModel>,
) -> MainMenu {
    let menu = gtk::gio::Menu::new();
    let help_menu = gtk::gio::Menu::new();
    let model_undo_menu = gtk::gio::Menu::new();
    {
        // #200: the only place undo and redo are visible. They name the
        // action they would reverse ("Undo Archive") and grey out when
        // there is nothing on the stack.
        menu.append_section(None, &model_undo_menu);

        let settings = gtk::gio::Menu::new();
        settings.append(Some(i18n("Settings").as_str()), Some("win.accounts"));
        menu.append_section(None, &settings);

        // The sidebar's account sections, for those who work from the
        // unified section alone (also Settings → Sidebar).
        let sidebar = gtk::gio::Menu::new();
        sidebar.append_item(&keyed_item(
            &i18n("Show Accounts"),
            "app.show-accounts",
            "<Ctrl><Shift>a",
        ));
        // Focus Mode: the distraction-free layout (also Ctrl+Shift+F
        // and Settings → Appearance, where its parts are chosen).
        sidebar.append(Some(i18n("Focus Mode").as_str()), Some("app.focus-mode"));
        sidebar.append(Some(i18n("Fullscreen").as_str()), Some("win.fullscreen"));
        menu.append_section(None, &sidebar);

        let printing = gtk::gio::Menu::new();
        printing.append(
            Some(i18n("Print Preview…").as_str()),
            Some("win.print-preview"),
        );
        printing.append(Some(i18n("Print Message…").as_str()), Some("win.print"));
        menu.append_section(None, &printing);

        menu.append_section(None, &help_menu);

        // Last, where a Quit item belongs.
        let quit = gtk::gio::Menu::new();
        quit.append(Some(i18n("Quit").as_str()), Some("app.quit"));
        menu.append_section(None, &quit);
    }
    MainMenu {
        menu,
        help: help_menu,
        undo: model_undo_menu,
        show_accounts_action: show_accounts_action(show_accounts, sender),
        focus_action: focus_mode_action(focus, sender),
    }
}

/// The "show-accounts" check item's action; flipping it is the Settings
/// switch's message.
fn show_accounts_action(
    show_accounts: bool,
    sender: &ComponentSender<AppModel>,
) -> gtk::gio::SimpleAction {
    let show_accounts_action =
        gtk::gio::SimpleAction::new_stateful("show-accounts", None, &show_accounts.to_variant());
    {
        let s = sender.clone();
        show_accounts_action.connect_change_state(move |action, value| {
            if let Some(on) = value.and_then(|v| v.get::<bool>()) {
                action.set_state(&on.to_variant());
                s.input(AppMsg::Pref(PrefOutput::SetShowAccounts(on)));
            }
        });
    }
    show_accounts_action
}

/// The "focus-mode" check item's action: turns Focus Mode on or off with
/// the parts saved in focus.toml.
fn focus_mode_action(
    focus: &config::FocusMode,
    sender: &ComponentSender<AppModel>,
) -> gtk::gio::SimpleAction {
    let focus_action =
        gtk::gio::SimpleAction::new_stateful("focus-mode", None, &focus.enabled.to_variant());
    {
        let s = sender.clone();
        focus_action.connect_change_state(move |action, value| {
            if let Some(on) = value.and_then(|v| v.get::<bool>()) {
                action.set_state(&on.to_variant());
                let mut focus = config::load_focus_mode();
                focus.enabled = on;
                s.input(AppMsg::Pref(PrefOutput::SetFocusMode(focus)));
            }
        });
    }
    focus_action
}
