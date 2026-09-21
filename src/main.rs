//! Hylki — a clean, fast, GNOME-native email client built with Rust + relm4.

mod app;
mod app_icon;
mod background;
mod avatar;
mod backend;
mod brand;
mod cache;
mod cloud;
mod color;
mod config;
mod console_log;
mod contacts;
mod datefmt;
mod desktop;
mod goa;
mod i18n;
mod invite;
mod legacy;
mod logo;
mod memory_report;
mod markdown;
mod models;
mod mutf7;
mod nautilus_ext;
mod notify;
mod oauth;
mod pgp;
mod platform;
mod power;
mod ram_cache;
mod reader;
mod rng;
mod spell;
mod theme;
mod theme_palettes;
mod tray;
mod unsubscribe;
mod ui;
mod verify;
mod worker;

use relm4::RelmApp;

use crate::app::AppModel;

pub const APP_ID: &str =
    if cfg!(feature = "beta") { "co.hyprlab.Hylki.Beta" } else { "co.hyprlab.Hylki" };

/// The user-visible application name.
pub const APP_NAME: &str = if cfg!(feature = "beta") { "Hylki (beta)" } else { "Hylki" };

/// The user-visible version — the crate version verbatim. Beta builds carry a
/// semver prerelease in Cargo.toml itself (e.g. "1.18.2-beta.2" on the beta
/// branch), so no suffix is bolted on here.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Counts what Rust code holds, for the memory section of an exported log.
#[global_allocator]
static ALLOCATOR: memory_report::CountingAllocator = memory_report::CountingAllocator;

/// Command-line flag for starting without a window (used by the autostart entry
/// the background portal writes).
pub const HIDDEN_FLAG: &str = "--hidden";
/// Set in the environment before the restart the welcome wizard's language
/// pick asks for, so the instance that comes back opens the wizard again
/// (the restart helper keeps it; app init clears it).
pub const WIZARD_AGAIN_VAR: &str = "HYLKI_WIZARD_AGAIN";

/// Whether this run started hidden. Read once the UI is built, to keep the first
/// activation from presenting the window that was deliberately not shown.
pub static HIDDEN_START: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() {
    // The allocator's tunables before the first allocation that matters (see
    // memory_report::tune_allocator).
    memory_report::tune_allocator();

    // Translations first: the text domain must be bound before any string
    // is shown, and the locale set before GTK sets its own.
    i18n::init();

    // Two log sinks: stderr honours RUST_LOG as before, and the console-mode
    // ring buffer (console_log.rs) always runs verbose so the status bar's
    // console has everything under the hood to show.
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::Layer;
        use tracing_subscriber::util::SubscriberInitExt;
        let stderr = tracing_subscriber::fmt::layer().with_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hylki=info".into()),
        );
        let console = tracing_subscriber::fmt::layer()
            .with_writer(console_log::ConsoleWriter)
            .with_ansi(false)
            .with_target(false)
            .compact()
            .with_filter(tracing_subscriber::EnvFilter::new("hylki=debug"));
        tracing_subscriber::registry().with(stderr).with(console).init();
    }

    // The restart helper (see app_icon.rs) never builds a UI: it waits for
    // the running instance to leave, then becomes the new one.
    if std::env::args().any(|a| a == app_icon::RESTART_FLAG) {
        app_icon::run_restart_helper();
    }

    legacy::migrate_dirs();
    legacy::tidy_host();
    // An AppImage has no fixed path for the GNOME Files extension to launch,
    // so it leaves one behind (#235).
    nautilus_ext::record_launcher();
    // Attachments the user opened in a previous session were decrypted to a temp
    // directory and left there. Clear it before anything else runs.
    ui::attachments_gallery::purge_attachment_dir();
    register_resources();
    // HYLKI_LOGO_PROBE=<address>[,<address>…] logs which source answers for a sender's
    // logo (BIMI, bundled, the site, or none), for checking one by hand.
    if let Ok(list) = std::env::var("HYLKI_LOGO_PROBE") {
        std::thread::spawn(move || {
            for email in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                tracing::info!("logo probe {email}: {}", logo::probe(email));
            }
        });
    }

    // `--hidden` starts without showing the window: the autostart entry written
    // by the background portal uses it, so logging in leaves Hylki checking mail
    // from the Background Apps menu rather than opening a window at you. The flag
    // is stripped before GTK sees the arguments, which would otherwise reject it
    // as unknown.
    let mut args: Vec<String> = std::env::args().collect();
    let hidden = args.iter().any(|a| a == HIDDEN_FLAG);
    args.retain(|a| a != HIDDEN_FLAG);
    HIDDEN_START.store(hidden, std::sync::atomic::Ordering::Relaxed);

    let adw_app = adw::Application::builder()
        .application_id(APP_ID)
        // mailto: links land here (the desktop file registers the scheme) —
        // both on a fresh launch and relayed from a second invocation.
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    {
        use gtk::gio::prelude::*;
        adw_app.connect_open(|app, files, _hint| {
            let mut attach_paths = Vec::new();
            for f in files {
                let uri = f.uri().to_string();
                tracing::info!("open: {}", if uri.starts_with("mailto:") { "mailto: URI" } else { uri.as_str() });
                if uri.starts_with("mailto:") {
                    app::queue_mailto(uri);
                } else if uri.starts_with("mid:") || uri.starts_with("MID:") {
                    // A Message-ID link (#130): open that message.
                    app::queue_mid(uri);
                } else if let Some(path) = f.path() {
                    // A file handed in from a file manager's "Open With" (or
                    // the command line): open a fresh composer with it
                    // attached, same as picking it from the attach dialog
                    // (Isaac's PR #96).
                    attach_paths.push(path);
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
        });
    }
    // The embedded icon gresource lives at /co/hyprlab/Hylki regardless of the
    // channel; pin the base path so the beta's app ID (co.hyprlab.Hylki.Beta)
    // doesn't derive a base the bundled symbolic icons aren't under.
    {
        use gtk::gio::prelude::ApplicationExt;
        adw_app.set_resource_base_path(Some("/co/hyprlab/Hylki"));
    }

    // A second launch (a mailto: link, or just opening the app again) must
    // hand off to the running instance and exit. RelmApp's run loop is built
    // for the primary only (a remote instance never leaves it), and the app
    // must NOT be registered early either — relm4 builds the whole UI in a
    // `startup` handler it connects inside run(), and registration is what
    // emits `startup`. So remoteness is checked bus-side, touching nothing.
    if primary_instance_running() && relay_to_primary(&args) {
        return;
    }

    // A brand-new install (no accounts, not the demo) is greeted by the
    // welcome wizard alone; the main window appears when the wizard finishes
    // or is closed (app.rs presents it from the wizard's hand-off).
    let first_run = std::env::var("HYLKI_DEMO").is_err()
        && config::load().unwrap_or_default().is_empty()
        && !config::wizard_completed();
    let app = RelmApp::from_app(adw_app)
        .with_args(args)
        .visible_on_activate(!hidden && !first_run);
    app.run::<AppModel>(());
}

/// Whether another instance already owns the app's D-Bus name.
pub(crate) fn primary_instance_running() -> bool {
    use gtk::glib::prelude::ToVariant;
    let Ok(conn) = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)
    else {
        return false;
    };
    conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(APP_ID,).to_variant()),
        None,
        gtk::gio::DBusCallFlags::NONE,
        3000,
        gtk::gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|v| v.get::<(bool,)>())
    .is_some_and(|(owned,)| owned)
}

/// Forward this invocation to the running primary instance and return once
/// it has been accepted: `Open` with any mailto:/mid:/file arguments, plain
/// `Activate` (present the window) otherwise.
///
/// The hand-off goes through a throwaway `GtkApplication` registered as a
/// remote instance rather than a hand-rolled D-Bus call, because of the
/// launcher's activation token (issue #187). GNOME hands it to us in
/// `XDG_ACTIVATION_TOKEN` (`DESKTOP_STARTUP_ID` on X11), and GTK 4 removes
/// both from the environment in a library constructor -- before `main` even
/// runs -- keeping the value for itself. Only GApplication's own remote
/// path puts that stashed token into the platform data of the `Activate`
/// call, and only with it does the primary's window complete the launch:
/// without it, GNOME Shell keeps the busy pointer and its "starting" state
/// for the whole 15 s timeout, during which clicking the icon again does
/// nothing at all. (The token is also what lets the window take the focus
/// from whoever launched us: Nautilus's "Send by email", a mailto: link in
/// a browser.)
///
/// Returns false when no primary answered after all (it quit between the
/// name check and the hand-off): the caller then starts normally.
fn relay_to_primary(args: &[String]) -> bool {
    use gtk::gio::prelude::ApplicationExt;
    let files: Vec<gtk::gio::File> = args
        .iter()
        .skip(1)
        .map(|a| {
            if a.starts_with("mailto:") || a.starts_with("mid:") || a.starts_with("MID:") {
                gtk::gio::File::for_uri(a)
            } else {
                // A plain path or a non-mailto URI (e.g. file://): the same
                // normalization GLib applies to HANDLES_OPEN arguments.
                gtk::gio::File::for_commandline_arg(a)
            }
        })
        .collect();
    let remote = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    if let Err(e) = remote.register(gtk::gio::Cancellable::NONE) {
        tracing::warn!("hand-off to the running instance failed: {e}");
        return false;
    }
    if !remote.is_remote() {
        // The name was free after all: we own it now, and releasing it
        // (dropping the object) lets the real app take it below.
        tracing::info!("the running instance left before the hand-off; starting instead");
        drop(remote);
        return false;
    }
    tracing::info!("handing off to the running instance ({} argument(s))", files.len());
    if files.is_empty() {
        remote.activate();
    } else {
        remote.open(&files, "");
    }
    // The call has been delivered (both are synchronous round trips). The
    // process ends right after this; letting the object go instead would
    // only print GLib's "did not unregister from D-Bus" warning to the
    // terminal, so it is deliberately left alive.
    std::mem::forget(remote);
    true
}

/// Register the embedded GResource holding Hylki's bundled symbolic icons.
///
/// The blob is compiled from `resources/hylki.gresource.xml` by `build.rs` and
/// baked into the binary. Registering it makes the icons available under the
/// resource path `/co/hyprlab/Hylki/icons`. Because the app's resource base
/// path is derived from `APP_ID`, GTK automatically appends that `icons`
/// subdirectory to the default icon theme's search path — so every
/// `co.hyprlab.Hylki-*-symbolic` name resolves from the bundle on any distro,
/// no filesystem install required.
fn register_resources() {
    use gtk::{gio, glib};
    let bytes = glib::Bytes::from_static(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/hylki.gresource"
    )));
    match gio::Resource::from_data(&bytes) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => tracing::error!("failed to register bundled icon resources: {e}"),
    }
    // The bundled sender logos (see src/logo.rs), under
    // /co/hyprlab/Hylki/logos/<source>/<file>.
    let logos = glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/logos.gresource")));
    match gio::Resource::from_data(&logos) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => tracing::error!("failed to register bundled logo resources: {e}"),
    }
}
