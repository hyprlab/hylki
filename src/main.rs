//! Hylki — a clean, fast, GNOME-native email client built with Rust + relm4.

// A binary crate exports nothing, so a bare `pub` only hides how far an item
// really reaches; `pub(crate)` or narrower says it.
#![warn(unreachable_pub)]

mod app;
mod backend;
mod cache;
mod color;
mod config;
mod contacts;
mod datefmt;
mod desktop;
mod encoding;
mod i18n;
mod mail;
mod models;
mod ram_cache;
mod rng;
mod services;
mod ui;
mod worker;

use relm4::RelmApp;

use crate::app::AppModel;
pub(crate) use crate::desktop::single_instance::primary_instance_running;
use crate::desktop::single_instance::{relay_to_primary, UriKind};

const APP_ID: &str = if cfg!(feature = "beta") {
    "co.hyprlab.Hylki.Beta"
} else {
    "co.hyprlab.Hylki"
};

/// The user-visible application name.
const APP_NAME: &str = if cfg!(feature = "beta") {
    "Hylki (beta)"
} else {
    "Hylki"
};

/// The user-visible version — the crate version verbatim. Beta builds carry a
/// semver prerelease in Cargo.toml itself (e.g. "1.18.2-beta.2" on the beta
/// branch), so no suffix is bolted on here.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Counts what Rust code holds, for the memory section of an exported log.
#[global_allocator]
static ALLOCATOR: desktop::memory_report::CountingAllocator =
    desktop::memory_report::CountingAllocator;

/// Command-line flag for starting without a window (used by the autostart entry
/// the background portal writes).
const HIDDEN_FLAG: &str = "--hidden";
/// Set in the environment before the restart the welcome wizard's language
/// pick asks for, so the instance that comes back opens the wizard again
/// (the restart helper keeps it; app init clears it).
const WIZARD_AGAIN_VAR: &str = "HYLKI_WIZARD_AGAIN";

/// Whether this run started hidden. Read once the UI is built, to keep the first
/// activation from presenting the window that was deliberately not shown.
static HIDDEN_START: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Marks a `GSK_RENDERER` that Hylki set itself, so a restart, which
/// inherits the environment, does not take it for the session's own.
const RENDERER_SET: &str = "HYLKI_GSK_RENDERER_SET";

/// Settings → System → Renderer (#403), into `GSK_RENDERER` before GTK
/// starts. One the session already has (a `flatpak override --env`) wins.
/// Left to GTK, an X server that cannot share memory gets Cairo (#547).
/// Single-threaded here, so setting the environment is safe.
fn apply_renderer() {
    if std::env::var_os(RENDERER_SET).is_some() {
        std::env::remove_var("GSK_RENDERER");
        std::env::remove_var(RENDERER_SET);
    }
    if std::env::var_os("GSK_RENDERER").is_some() {
        return;
    }
    let mut renderer = config::load_renderer();
    if !renderer.is_empty() {
        tracing::info!("renderer: {renderer}, from Settings");
    } else if desktop::x11_shm::gtk_uses_x11() && desktop::x11_shm::attach_fails() {
        tracing::info!("renderer: cairo, the X server cannot attach shared memory (#547)");
        renderer = "cairo".into();
    } else {
        return;
    }
    std::env::set_var("GSK_RENDERER", &renderer);
    std::env::set_var(RENDERER_SET, "1");
}

fn main() {
    // The allocator's tunables before the first allocation that matters (see
    // memory_report::tune_allocator).
    desktop::memory_report::tune_allocator();

    // Translations first: the text domain must be bound before any string
    // is shown, and the locale set before GTK sets its own.
    i18n::init();
    init_tracing();

    // The restart helper (see app_icon.rs) never builds a UI: it waits for
    // the running instance to leave, then becomes the new one.
    if std::env::args().any(|a| a == desktop::app_icon::RESTART_FLAG) {
        desktop::app_icon::run_restart_helper();
    }

    desktop::legacy::migrate_dirs();
    desktop::legacy::tidy_host();
    // An AppImage has no fixed path for the GNOME Files extension to launch,
    // so it leaves one behind (#235).
    desktop::nautilus_ext::record_launcher();
    // Attachments the user opened in a previous session were decrypted to a temp
    // directory and left there. Clear it before anything else runs.
    ui::attachment_io::purge_attachment_dir();
    register_resources();
    start_logo_probe();

    let (args, hidden) = strip_hidden_flag(std::env::args().collect());
    HIDDEN_START.store(hidden, std::sync::atomic::Ordering::Relaxed);

    apply_renderer();
    // On X11 the window class is the program name, which defaults to the
    // binary's, "hylki"; docks match it against the launcher's
    // StartupWMClass, the app ID, and showed a second icon without it (#402).
    // Wayland takes the app ID from the application, so nothing changes there.
    // The display name keeps sound mixers from showing the app ID instead.
    gtk::glib::set_prgname(Some(APP_ID));
    gtk::glib::set_application_name("Hylki");

    let adw_app = build_application();

    // A second launch (a mailto: link, or just opening the app again) must
    // hand off to the running instance and exit. RelmApp's run loop is built
    // for the primary only (a remote instance never leaves it), and the app
    // must NOT be registered early either — relm4 builds the whole UI in a
    // `startup` handler it connects inside run(), and registration is what
    // emits `startup`. So remoteness is checked bus-side, touching nothing.
    if primary_instance_running() && relay_to_primary(&args) {
        return;
    }

    let first_run = is_first_run();
    install_startup_handlers(&adw_app);
    let app = RelmApp::from_app(adw_app)
        .with_args(args)
        .visible_on_activate(!hidden && !first_run);
    app.run::<AppModel>(());
    app::finish_keyring_writes();
}

/// Two log sinks: stderr honours RUST_LOG as before, and the console-mode
/// ring buffer (console_log.rs) always runs verbose so the status bar's
/// console has everything under the hood to show.
fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;
    let stderr = tracing_subscriber::fmt::layer().with_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "hylki=info".into()),
    );
    let console = tracing_subscriber::fmt::layer()
        .with_writer(desktop::console_log::ConsoleWriter)
        .with_ansi(false)
        .with_target(false)
        .compact()
        .with_filter(tracing_subscriber::EnvFilter::new("hylki=debug"));
    tracing_subscriber::registry()
        .with(stderr)
        .with(console)
        .init();
}

/// HYLKI_LOGO_PROBE=<address>[,<address>…] logs which source answers for a sender's
/// logo (BIMI, bundled, the site, or none), for checking one by hand.
fn start_logo_probe() {
    if let Ok(list) = std::env::var("HYLKI_LOGO_PROBE") {
        std::thread::spawn(move || {
            for email in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                tracing::info!("logo probe {email}: {}", services::logo::probe(email));
            }
        });
    }
}

/// `--hidden` starts without showing the window: the autostart entry written
/// by the background portal uses it, so logging in leaves Hylki checking mail
/// from the Background Apps menu rather than opening a window at you. The flag
/// is stripped before GTK sees the arguments, which would otherwise reject it
/// as unknown. Returns the arguments without it, and whether it was there.
fn strip_hidden_flag(mut args: Vec<String>) -> (Vec<String>, bool) {
    let hidden = args.iter().any(|a| a == HIDDEN_FLAG);
    args.retain(|a| a != HIDDEN_FLAG);
    (args, hidden)
}

/// The application, set up to receive opened files and links.
fn build_application() -> adw::Application {
    let adw_app = adw::Application::builder()
        .application_id(APP_ID)
        // mailto: links land here (the desktop file registers the scheme) —
        // both on a fresh launch and relayed from a second invocation.
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    {
        use gtk::gio::prelude::*;
        adw_app.connect_open(open_files);
    }
    // The embedded icon gresource lives at /co/hyprlab/Hylki regardless of the
    // channel; pin the base path so the beta's app ID (co.hyprlab.Hylki.Beta)
    // doesn't derive a base the bundled symbolic icons aren't under.
    {
        use gtk::gio::prelude::ApplicationExt;
        adw_app.set_resource_base_path(Some("/co/hyprlab/Hylki"));
    }
    adw_app
}

/// The `open` signal: links, saved messages and files handed to the app,
/// on a fresh launch or relayed from a second one.
fn open_files(app: &adw::Application, files: &[gtk::gio::File], _hint: &str) {
    use gtk::gio::prelude::*;
    let mut attach_paths = Vec::new();
    for f in files {
        let uri = f.uri().to_string();
        let kind = UriKind::of(&uri);
        tracing::info!(
            "open: {}",
            if kind == UriKind::Mailto {
                "mailto: URI"
            } else {
                uri.as_str()
            }
        );
        match kind {
            UriKind::Mailto => app::queue_mailto(uri),
            // A Message-ID link (#130): open that message.
            UriKind::Mid => app::queue_mid(uri),
            UriKind::Other => {
                if let Some(path) = f.path().filter(|p| app::is_message_file(p)) {
                    // A saved message (#398): read it, rather than attach it.
                    app::queue_message_file(path);
                } else if let Some(path) = f.path() {
                    // A file handed in from a file manager's "Open With" (or
                    // the command line): open a fresh composer with it
                    // attached, same as picking it from the attach dialog
                    // (Isaac's PR #96).
                    attach_paths.push(path);
                }
            }
        }
    }
    if !attach_paths.is_empty() {
        app::queue_attach_files(attach_paths);
    }
    // `open` replaces `activate` when a URI is passed: activate
    // explicitly so the window (and on first launch, the whole UI)
    // still comes up, with the composer opening over it. The signal
    // is emitted directly rather than through `app.activate()`:
    // GApplication brackets that call with its own before_emit,
    // whose platform data (built from this primary's environment)
    // carries no activation token and so wipes the launcher's token
    // the `Open` call has just installed on the display. The window
    // would then present itself with nothing to hand the compositor
    // (#187: busy pointer until GNOME's 15 s timeout, no focus).
    use gtk::glib::prelude::ObjectExt;
    app.emit_by_name::<()>("activate", &[]);
}

/// A brand-new install (no accounts, not the demo) is greeted by the
/// welcome wizard alone; the main window appears when the wizard finishes
/// or is closed (app/accounts.rs presents it from the wizard's hand-off).
fn is_first_run() -> bool {
    std::env::var("HYLKI_DEMO").is_err()
        && config::load().unwrap_or_default().is_empty()
        && !config::wizard_completed()
}

/// Handlers that must run in GTK's `startup`, before relm4's builds a window.
fn install_startup_handlers(adw_app: &adw::Application) {
    use gtk::gio::prelude::ApplicationExt;
    // Every window's text fields open their emoji chooser at the cursor.
    adw_app.connect_startup(|_| crate::ui::emoji::follow_all());
    // GTK takes its direction from its own gtk40 catalogue ("default:LTR"
    // translated as "default:RTL"), which a Flatpak runtime or a minimal
    // install often lacks, and which a language chosen in Settings never
    // reaches (#366). So the direction is set from the language here, after
    // GTK's own init (the startup class handler runs first) and before
    // relm4's startup handler builds any window.
    adw_app.connect_startup(|_| {
        if i18n::ui_is_rtl() {
            gtk::Widget::set_default_direction(gtk::TextDirection::Rtl);
        }
        i18n::set_rtl(gtk::Widget::default_direction() == gtk::TextDirection::Rtl);
    });
}

/// Register the embedded GResource holding Hylki's bundled symbolic icons.
///
/// The blob is compiled from `resources/hylki.gresource.xml` by `build.rs` and
/// baked into the binary. Registering it makes the icons available under the
/// resource path `/co/hyprlab/Hylki/icons`. Because the app's resource base
/// path is derived from `APP_ID`, GTK automatically appends that `icons`
/// subdirectory to the default icon theme's search path, as part of hicolor.
/// Icons are named plainly (`edit-find-symbolic`), so the user's icon theme
/// draws any it has, and the bundle supplies the rest on any distro: the
/// app's own icons, and GNOME's for a theme that lacks them.
fn register_resources() {
    use gtk::{gio, glib};
    let bytes =
        glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/hylki.gresource")));
    match gio::Resource::from_data(&bytes) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => tracing::error!("failed to register bundled icon resources: {e}"),
    }
    // The bundled sender logos (see src/services/logo.rs), under
    // /co/hyprlab/Hylki/logos/<source>/<file>.
    let logos =
        glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/logos.gresource")));
    match gio::Resource::from_data(&logos) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => tracing::error!("failed to register bundled logo resources: {e}"),
    }
    // The built-in new-mail sounds, under /co/hyprlab/Hylki/sounds/<name>.ogg.
    let sounds = glib::Bytes::from_static(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/sounds.gresource"
    )));
    match gio::Resource::from_data(&sounds) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => tracing::error!("failed to register bundled sound resources: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hidden_flag_is_taken_out_wherever_it_stands() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            strip_hidden_flag(args(&["hylki", "--hidden", "mailto:a@example.org"])),
            (args(&["hylki", "mailto:a@example.org"]), true)
        );
        assert_eq!(
            strip_hidden_flag(args(&["hylki", "--hidden", "x", "--hidden"])),
            (args(&["hylki", "x"]), true)
        );
        assert_eq!(
            strip_hidden_flag(args(&["hylki"])),
            (args(&["hylki"]), false)
        );
        // Only the exact flag.
        assert_eq!(
            strip_hidden_flag(args(&["hylki", "--hidden=1"])),
            (args(&["hylki", "--hidden=1"]), false)
        );
    }
}
