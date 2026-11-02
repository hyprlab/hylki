//! The main window: its size and breakpoints, and the app and window actions.

use adw::prelude::*;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::prelude::*;

use crate::config;

use super::accounts::finish_keyring_writes;
use super::model::AppModel;
use super::msg::AppMsg;
use super::{
    AboutAction, AccountsAction, AppModelWidgets, ConsoleAction, FindAction, FullscreenAction,
    PreferencesAction, PrintAction, PrintPreviewAction, RedoAction, ShortcutsAction,
    StatusBarAction, UndoAction, WindowActionGroup,
};

/// Saves the window's size and maximized state. Only the geometry: on
/// Wayland the compositor owns placement and the monitor. A maximized
/// window keeps the size saved before it was maximized, and a fullscreen
/// one leaves the saved state alone: its size is the monitor's.
pub(super) fn save_window_geometry(w: &adw::ApplicationWindow) {
    if w.is_fullscreen() {
        return;
    }
    let maximized = w.is_maximized();
    let (width, height) = if maximized {
        let (sw, sh, _) = crate::config::load_window_state();
        (sw, sh)
    } else {
        (w.width(), w.height())
    };
    crate::config::save_window_state(width, height, maximized);
}

/// Exits the process the moment state is saved. Letting GTK, WebKit and
/// the per-account worker threads tear down the normal way can abort — a
/// Rust panic fired from a GObject dispose callback becomes SIGABRT, which
/// the Flatpak surfaces as a crash notification. Nothing else needs
/// persisting on quit: accounts and settings are written as they change,
/// held sends are parked, the keyring's share once its thread is done.
pub(super) fn exit_now(outgoing: &super::outgoing::OutgoingList) -> ! {
    super::outgoing::park_held(outgoing);
    finish_keyring_writes();
    std::process::exit(0)
}

impl AppModel {
    /// Restore the last window size + maximized state (Wayland can't restore
    /// position/monitor).
    pub(super) fn init_window_size(
        &self,
        root: &adw::ApplicationWindow,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let (win_w, win_h, win_max) = config::load_window_state();
        root.set_default_size(win_w, win_h);
        if win_max {
            root.maximize();
        }
        if self.start_fullscreen {
            root.fullscreen();
        }
        // Below this width the expanded sidebar and a full-width Actions
        // Palette can't both fit (280 sidebar + 324 list + 492 reader), so the
        // sidebar drops to its icon rail automatically — this is what keeps the
        // palette whole when the window is tiled to half of a 1920px screen.
        // With a breakpoint present the window no longer derives its minimum
        // size from its content, so pin an explicit floor: the sidebar rail
        // (80) + the list's palette floor (324) + the reader header (~492).
        root.set_size_request(904, 360);
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            1094.0,
            adw::LengthUnit::Px,
        ));
        {
            let s = sender.clone();
            narrow.connect_apply(move |_| s.input(AppMsg::AutoRail(true)));
            let s = sender.clone();
            narrow.connect_unapply(move |_| s.input(AppMsg::AutoRail(false)));
        }
        root.add_breakpoint(narrow);
        {
            let s = sender.clone();
            let guard = self.peek_transition.clone();
            widgets
                .peek_split
                .connect_show_sidebar_notify(move |split| {
                    tracing::info!(
                        "peek: show-sidebar notify shown={} guarded={}",
                        split.shows_sidebar(),
                        guard.get()
                    );
                    // Only a *user* dismissal (scrim click / swipe) counts — our
                    // own open/close transitions notify too.
                    if !guard.get() && !split.shows_sidebar() {
                        s.input(AppMsg::SidebarPeekDismissed);
                    }
                });
        }
    }

    /// GNOME's Background Apps menu quits an app by activating its `quit`
    /// action over D-Bus, falling back to `flatpak kill` if nothing answers
    /// within five seconds. Without this the ✕ there would be a hard kill.
    pub(super) fn install_app_actions(
        &self,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let app = relm4::main_application();
        app.add_action(&self.show_accounts_action);
        app.add_action(&self.focus_action);
        app.add_action(&self.reading_pane_action);
        let window = root.clone();
        let outgoing = self.outgoing.clone();
        let quit = gtk::gio::SimpleAction::new("quit", None);
        quit.connect_activate(move |_, _| {
            // Same teardown as closing the last window: save the geometry,
            // then exit rather than unwinding through GTK and WebKit.
            save_window_geometry(&window);
            crate::desktop::background::set_status("");
            exit_now(&outgoing)
        });
        app.add_action(&quit);
        gtk::prelude::GtkApplicationExt::set_accels_for_action(&app, "app.quit", &["<Ctrl>q"]);
        // Ctrl+Shift+A shows or hides the sidebar's account sections and
        // Ctrl+Shift+C opens the console, but in a composer they are
        // Thunderbird's Attach and Cc (#490, #491). An application key
        // reaches the main window before the composer in it, so the two
        // belong to actions of their own that ask where the focus is
        // (AppMsg::ComposerKey). The menus keep the real actions, with
        // the keys shown beside them, so a click there never reaches a
        // composer that had the focus before.
        for (name, keyval, accel) in [
            ("accounts-key", gtk::gdk::Key::a, "<Ctrl><Shift>a"),
            ("console-key", gtk::gdk::Key::c, "<Ctrl><Shift>c"),
        ] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let s = sender.clone();
            action.connect_activate(move |_, _| s.input(AppMsg::ComposerKey(keyval)));
            app.add_action(&action);
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                &format!("app.{name}"),
                &[accel],
            );
        }
        // Ctrl+Alt+1, 2 and 3: the reader right of the list, below it, or
        // off (#475). The menu shows them beside its radio items.
        for (pane, accel) in [
            (config::ReadingPane::Right, "<Ctrl><Alt>1"),
            (config::ReadingPane::Below, "<Ctrl><Alt>2"),
            (config::ReadingPane::Off, "<Ctrl><Alt>3"),
        ] {
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                &format!("app.reading-pane::{}", pane.key()),
                &[accel],
            );
        }
        // Ctrl+Shift+F switches Focus Mode on and off.
        gtk::prelude::GtkApplicationExt::set_accels_for_action(
            &app,
            "app.focus-mode",
            &["<Ctrl><Shift>f"],
        );
        // Ctrl+ / Ctrl- / Ctrl+0: message zoom, the bodies alone. The
        // = key stands in for + on layouts where + needs Shift, and the
        // keypad's keys count too, as in every browser.
        for (name, step, keys) in [
            (
                "zoom-in",
                1i8,
                &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"][..],
            ),
            ("zoom-out", -1, &["<Ctrl>minus", "<Ctrl>KP_Subtract"][..]),
            ("zoom-reset", 0, &["<Ctrl>0", "<Ctrl>KP_0"][..]),
        ] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let s = sender.clone();
            action.connect_activate(move |_, _| s.input(AppMsg::ZoomKey(step)));
            app.add_action(&action);
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                &format!("app.{name}"),
                keys,
            );
        }
        // Ctrl+W closes the window only (issue #64): with "run in the
        // background" on, mail keeps arriving — unlike Ctrl+Q, which
        // quits outright. GTK's built-in window.close action does
        // exactly the same as the titlebar's close button.
        gtk::prelude::GtkApplicationExt::set_accels_for_action(&app, "window.close", &["<Ctrl>w"]);

        // Activating the app again — its icon, a notification, or the
        // autostart entry — brings the hidden window back rather than doing
        // nothing.
        let window = root.clone();
        // Started hidden? Then the first activation is this very launch, and
        // presenting here would undo it. Every activation after that is a
        // person asking for the window.
        let pending_hidden_start =
            std::cell::Cell::new(crate::HIDDEN_START.load(std::sync::atomic::Ordering::Relaxed));
        app.connect_activate(move |_| {
            if pending_hidden_start.replace(false) {
                return;
            }
            crate::desktop::session::present_window(&window);
        });
    }
}

/// The window's action group (Settings, About, print, find, undo and redo
/// ...) and their accelerators. Undo and Redo are handed back for the model
/// to enable and disable.
pub(super) fn install_window_actions(
    root: &adw::ApplicationWindow,
    sender: &ComponentSender<AppModel>,
) -> (RelmAction<UndoAction>, RelmAction<RedoAction>) {
    let mut group = RelmActionGroup::<WindowActionGroup>::new();
    let accounts_sender = sender.clone();
    group.add_action(RelmAction::<AccountsAction>::new_stateless(move |_| {
        accounts_sender.input(AppMsg::OpenSettings);
    }));
    let prefs_sender = sender.clone();
    group.add_action(RelmAction::<PreferencesAction>::new_stateless(move |_| {
        prefs_sender.input(AppMsg::OpenPreferences);
    }));
    let about_sender = sender.clone();
    group.add_action(RelmAction::<AboutAction>::new_stateless(move |_| {
        about_sender.input(AppMsg::OpenAbout);
    }));
    let shortcuts_sender = sender.clone();
    group.add_action(RelmAction::<ShortcutsAction>::new_stateless(move |_| {
        shortcuts_sender.input(AppMsg::ShowShortcuts);
    }));
    let print_sender = sender.clone();
    group.add_action(RelmAction::<PrintAction>::new_stateless(move |_| {
        print_sender.input(AppMsg::PrintMessage);
    }));
    let preview_sender = sender.clone();
    group.add_action(RelmAction::<PrintPreviewAction>::new_stateless(move |_| {
        preview_sender.input(AppMsg::PrintPreview);
    }));
    // The status bar reveals itself for errors; this is the manual path
    // (the dedicated button is gone from the sidebar).
    let status_sender = sender.clone();
    {
        let s = sender.clone();
        group.add_action(RelmAction::<ConsoleAction>::new_stateless(move |_| {
            s.input(AppMsg::OpenConsole);
        }));
    }
    {
        let s = sender.clone();
        group.add_action(RelmAction::<FindAction>::new_stateless(move |_| {
            s.input(AppMsg::OpenListSearch);
        }));
    }
    let undo_action = {
        let s = sender.clone();
        RelmAction::<UndoAction>::new_stateless(move |_| s.input(AppMsg::Undo))
    };
    let redo_action = {
        let s = sender.clone();
        RelmAction::<RedoAction>::new_stateless(move |_| s.input(AppMsg::Redo))
    };
    group.add_action(undo_action.clone());
    group.add_action(redo_action.clone());
    group.add_action(RelmAction::<StatusBarAction>::new_stateless(move |_| {
        status_sender.input(AppMsg::ToggleNotifications);
    }));
    // A window action, so F11 in a message's own window leaves the main
    // window as it is. The state follows the window rather than the
    // other way round: the compositor can leave fullscreen on its own.
    let fullscreen = {
        let window = root.clone();
        RelmAction::<FullscreenAction>::new_stateful(&root.is_fullscreen(), move |_, on| {
            *on = !window.is_fullscreen();
            if *on {
                window.fullscreen();
            } else {
                window.unfullscreen();
            }
        })
    };
    {
        let action = fullscreen.gio_action().clone();
        root.connect_fullscreened_notify(move |w| {
            action.set_state(&w.is_fullscreen().to_variant());
        });
    }
    group.add_action(fullscreen);
    group.register_for_widget(root);

    // A real accelerator rather than a key handler: GTK matches these before
    // the keystroke reaches whatever has focus, so Ctrl+? works while reading
    // a message (the web view would otherwise swallow it). Both spellings are
    // bound because layouts disagree about whether Ctrl+Shift+/ arrives as
    // `question` or as `slash`, and F1 is the GNOME convention.
    relm4::main_application().set_accelerators_for_action::<PrintAction>(&["<Ctrl>p"]);
    relm4::main_application()
        .set_accelerators_for_action::<PrintPreviewAction>(&["<Ctrl><Shift>p"]);
    relm4::main_application().set_accelerators_for_action::<StatusBarAction>(&["<Ctrl><Shift>s"]);
    relm4::main_application().set_accelerators_for_action::<FindAction>(&["<Ctrl>f"]);
    relm4::main_application().set_accelerators_for_action::<FullscreenAction>(&["F11"]);
    // On the menu's action, so the menu shows the keys beside Settings
    // (#513).
    relm4::main_application().set_accelerators_for_action::<AccountsAction>(&["<Ctrl>comma"]);
    relm4::main_application().set_accelerators_for_action::<ShortcutsAction>(&[
        "<Ctrl>question",
        "<Ctrl><Shift>question",
        "<Ctrl>slash",
        "<Ctrl><Shift>slash",
        "F1",
    ]);
    (undo_action, redo_action)
}
