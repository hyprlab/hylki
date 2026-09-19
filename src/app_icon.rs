//! The app icon the user chose, and how it reaches the desktop.
//!
//! The app ships one icon (the Hylki envelope, `data/icons/src/default.svg`)
//! and carries a gallery of alternative envelopes inside the binary. The
//! beta ships the default's `.Devel` twin, GNOME's development-build
//! styling (the hazard stripe). A choice is applied by writing that artwork
//! over the app's icon name in the user's own icon directory
//! (`~/.local/share/icons/hicolor`), which every desktop searches before
//! the install's — the Flatpak export included — so the dock, app grid and
//! switcher pick it up under the same name. "Default" removes the override
//! and whatever the build installed shows again. Fresh installs pick in the
//! welcome wizard.
//!
//! The tray icon and the in-app uses draw the same choice, so it needs no
//! restart; the window's own icon (X11 fallback, some panels) does, and a
//! restart is offered — see [`launch_restart_helper`].

use std::path::PathBuf;

use crate::i18n::i18n_noop;

/// One gallery entry: the id stored in settings, its label, and the art.
pub struct IconChoice {
    pub id: &'static str,
    pub label: &'static str,
    pub png: &'static [u8],
}

/// The id meaning "whatever this build ships".
pub const DEFAULT_ID: &str = "default";
/// Bumped when a release's new default icon is to replace every existing
/// choice once (1.23's blue bird envelope was generation 1; the Hylki
/// envelope of 1.35, the first release under the new name, is 2): the
/// first start on such a release resets the stored choice to the default,
/// and records the generation so a choice made afterwards stands.
pub const ICON_GENERATION: u32 = 2;

/// The icon this build installs under its app ID.
#[cfg(not(feature = "beta"))]
const DEFAULT_PNG: &[u8] = include_bytes!("../data/icons/hicolor/512x512/apps/co.hyprlab.Hylki.png");
#[cfg(feature = "beta")]
const DEFAULT_PNG: &[u8] =
    include_bytes!("../data/icons/hicolor/512x512/apps/co.hyprlab.Hylki.Beta.png");

macro_rules! alt {
    ($id:literal, $label:literal) => {
        IconChoice {
            id: $id,
            label: i18n_noop($label),
            png: include_bytes!(concat!("../data/icons/alt/", $id, ".png")),
        }
    };
}

/// The gallery, in display order: the build's own icon (the Hylki
/// envelope, so it has no entry of its own), the plain envelopes, the two
/// with the bird, the default drawn at full size rather than to the GNOME
/// icon grid, and the classic icon last.
const CATALOG: &[IconChoice] = &[
    IconChoice { id: DEFAULT_ID, label: i18n_noop("Default"), png: DEFAULT_PNG },
    alt!("envelope-blue", "Blue"),
    alt!("envelope-yellow", "Yellow"),
    alt!("envelope-white", "White"),
    alt!("envelope-manilla", "Manila"),
    alt!("envelope-faded-blue", "Faded blue"),
    alt!("envelope-starfield", "Starfield"),
    alt!("envelope-bird-blue", "Blue with bird"),
    alt!("envelope-bird-yellow", "Yellow with bird"),
    alt!("non-hig", "Full size"),
    alt!("classic", "Classic"),
];

/// Every choice the gallery offers.
pub fn catalog() -> impl Iterator<Item = &'static IconChoice> {
    CATALOG.iter()
}

/// Normalise a stored id to one this build offers. Every id from the
/// Hylki galleries (birds, colours, patterns, the classic envelope) is
/// gone; a stored one falls back to the default, which generation 2 puts
/// on every install once anyway.
fn effective(id: &str) -> &'static str {
    catalog().find(|c| c.id == id).map(|c| c.id).unwrap_or(DEFAULT_ID)
}

/// The artwork for an id (the default's for an unknown one).
pub fn png_for(id: &str) -> &'static [u8] {
    let id = effective(id);
    CATALOG.iter().find(|c| c.id == id).map(|c| c.png).unwrap_or(DEFAULT_PNG)
}

/// The choice in force, settling it on the first start that finds none
/// (the default; the wizard lets a fresh install pick). The override on
/// disk is brought in line either way, so a reinstall (or a changed
/// default) never silently swaps the icon someone chose.
pub fn init_on_startup() -> String {
    if std::env::var("HYLKI_DEMO").is_ok() {
        return DEFAULT_ID.to_string();
    }
    let id = if crate::config::load_app_icon_generation() < ICON_GENERATION {
        // A release with a new authoritative icon: everyone gets it once,
        // over whatever the previous install had chosen or kept.
        crate::config::save_app_icon(DEFAULT_ID);
        crate::config::save_app_icon_generation(ICON_GENERATION);
        DEFAULT_ID.to_string()
    } else {
        match crate::config::load_app_icon() {
            Some(id) => id,
            None => {
                crate::config::save_app_icon(DEFAULT_ID);
                DEFAULT_ID.to_string()
            }
        }
    };
    let id = effective(&id).to_string();
    apply(&id);
    id
}

/// Persist a choice and put it on the desktop.
pub fn set(id: &str) -> String {
    let id = effective(id).to_string();
    crate::config::save_app_icon(&id);
    if std::env::var("HYLKI_DEMO").is_err() {
        apply(&id);
    }
    id
}

/// The icon name a choice is written under: `<APP_ID>-<choice>`, the
/// default included. A fresh name per choice is deliberate — GNOME Shell
/// caches app icons by name for the life of the session and never re-reads
/// a file it has already shown, so swapping the art under one name shows
/// nothing until the next login.
pub fn icon_name(id: &str) -> String {
    format!("{}-{id}", crate::APP_ID)
}

/// Whether this process runs inside a Flatpak sandbox.
fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// The user's home — the host's, from inside the sandbox too.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir)
}

/// The user's share directory as the *desktop* sees it. Inside Flatpak
/// XDG_DATA_HOME is the sandbox's private dir, so the host's default is
/// used (that is where the manifest mounts `xdg-data/icons/hicolor` and
/// `xdg-data/applications`).
fn host_data_home() -> Option<PathBuf> {
    if !in_flatpak() {
        if let Some(d) = dirs::data_dir() {
            return Some(d);
        }
    }
    Some(home()?.join(".local/share"))
}

fn hicolor_dir() -> Option<PathBuf> {
    Some(host_data_home()?.join("icons/hicolor"))
}

/// The icon sizes the install ships — every one is written under the
/// chosen name, so whichever a desktop asks for resolves to the choice.
const SIZES: [i32; 2] = [512, 256];

/// Put a choice on the desktop: the art under its own name in the user's
/// icon directory, and the launcher pointed at that file.
///
/// The launcher names the file by absolute path, never by icon name. The
/// desktop then loads it directly, outside its icon-theme cache — which
/// GNOME Shell only re-scans on its own schedule: a freshly written name
/// stays unknown for seconds, and a removed file keeps resolving to its
/// old path (drawn as a blank) until the next re-scan. A changed path also
/// counts as a changed launcher, so the dock rebuilds the icon at once.
fn apply(id: &str) {
    let name = icon_name(id);
    let Some(hicolor) = hicolor_dir() else { return };
    let launcher = Launcher::find();
    let is_default = id == DEFAULT_ID;
    // The default needs no file of its own where the install's launcher
    // can simply be restored (the copy is removed); a launcher the user's
    // own install owns keeps pointing at a file, so its default is written
    // like any other choice.
    let own_file = !is_default || matches!(launcher, Launcher::Owned(_));
    sync_icon_files(&hicolor, id, &name, own_file);
    let path = hicolor.join("512x512/apps").join(format!("{name}.png"));
    launcher.point_at(&path.to_string_lossy(), is_default && !own_file);
    // Hylki's own windows (X11, panels that draw window icons).
    if own_file {
        gtk::Window::set_default_icon_name(&name);
    } else {
        gtk::Window::set_default_icon_name(crate::APP_ID);
    }
}

/// Write the current choice's art (if it is not already there) and clear
/// the art of earlier choices, then nudge the directories' timestamps so
/// icon caches re-scan them.
fn sync_icon_files(hicolor: &std::path::Path, id: &str, name: &str, write: bool) {
    let prefix = format!("{}-", crate::APP_ID);
    let bare = format!("{}.png", crate::APP_ID);
    // The bare-name override an earlier build wrote over the shipped icon
    // is cleared too — but only when it is one of ours (judged by the 512,
    // the unscaled art): a native install keeps its own copy of the
    // shipped icon there.
    let bare_is_ours = is_alt_art(&hicolor.join("512x512/apps").join(&bare));
    let mut changed = false;
    for size in SIZES {
        let dir = hicolor.join(format!("{size}x{size}/apps"));
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                let stale = (fname.starts_with(&prefix)
                    && fname.ends_with(".png")
                    && fname != format!("{name}.png"))
                    || (fname == bare && bare_is_ours);
                if stale && std::fs::remove_file(entry.path()).is_ok() {
                    changed = true;
                }
            }
        }
        if !write {
            continue;
        }
        let path = dir.join(format!("{name}.png"));
        let png = png_for(id);
        let bytes = if size == 512 {
            std::borrow::Cow::Borrowed(png)
        } else {
            match scaled_png(png, size) {
                Some(b) => std::borrow::Cow::Owned(b),
                None => continue,
            }
        };
        if std::fs::read(&path).map(|cur| cur == *bytes).unwrap_or(false) {
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("could not create {}: {e}", dir.display());
            continue;
        }
        match std::fs::write(&path, &*bytes) {
            Ok(()) => changed = true,
            Err(e) => tracing::warn!("could not write {}: {e}", path.display()),
        }
    }
    if changed {
        touch(hicolor);
        if let Some(parent) = hicolor.parent() {
            touch(parent);
        }
    }
}

/// Whether a file is one of the gallery's non-default icons, byte for byte.
fn is_alt_art(path: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    CATALOG.iter().any(|c| c.id != DEFAULT_ID && c.png == bytes.as_slice())
}

/// The artwork re-encoded at `size`.
fn scaled_png(png: &[u8], size: i32) -> Option<Vec<u8>> {
    use gtk::gdk_pixbuf::{InterpType, PixbufLoader};
    use gtk::prelude::PixbufLoaderExt;
    let loader = PixbufLoader::with_type("png").ok()?;
    loader.write(png).ok()?;
    loader.close().ok()?;
    let pb = loader.pixbuf()?.scale_simple(size, size, InterpType::Hyper)?;
    pb.save_to_bufferv("png", &[]).ok()
}

fn touch(dir: &std::path::Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

// ---------------------------------------------------------------------------
// The launcher
// ---------------------------------------------------------------------------

/// Marker key in a launcher Hylki wrote, so only its own files are ever
/// removed or regenerated.
const LAUNCHER_MARK: &str = "X-Hylki-Icon-Launcher";

/// When the launcher was last written by this process.
static LAST_LAUNCHER_WRITE: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// How long the desktop takes to act on a rewritten launcher. GNOME Shell
/// rate-limits its watch on the applications directory and then waits a
/// further five seconds before reloading; only then does it replace the
/// app object a new window will be bound to. A restart that lands inside
/// that window binds the new window to the old object — and the dock
/// shows the old icon.
const LAUNCHER_SETTLE: std::time::Duration = std::time::Duration::from_secs(9);

fn note_launcher_written() {
    *LAST_LAUNCHER_WRITE.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(std::time::Instant::now());
}

/// How much longer to keep running before a restart, so the desktop has
/// acted on the launcher written for the current choice. Zero when it has
/// had the time, or nothing was written.
pub fn launcher_settle_remaining() -> std::time::Duration {
    let last = *LAST_LAUNCHER_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    match last {
        Some(t) => LAUNCHER_SETTLE.saturating_sub(t.elapsed()),
        None => std::time::Duration::ZERO,
    }
}

/// How the app's launcher can be pointed at an icon. The desktop resolves
/// an app's icon through its `.desktop` entry, and a per-user copy of that
/// entry in `~/.local/share/applications` takes precedence over the
/// installed one — the mechanism menu editors use.
enum Launcher {
    /// Flatpak and packaged installs: a copy of the installed entry,
    /// carrying the chosen icon and everything else verbatim, that hides
    /// itself once the app is gone (`TryExec`).
    Shadow {
        base: PathBuf,
        user_path: PathBuf,
        /// Already one of ours.
        ours: bool,
        /// The `Exec` line Flatpak exports (the sandbox copy's is bare).
        exec: Option<String>,
        try_exec: String,
        flatpak: bool,
    },
    /// A launcher the user's own install put there (install.sh, a source
    /// tree): only its `Icon=` line is ever touched.
    Owned(PathBuf),
    /// No installed launcher at all.
    None,
}

impl Launcher {
    fn find() -> Self {
        let file = format!("{}.desktop", crate::APP_ID);
        let Some(user_dir) = host_data_home().map(|d| d.join("applications")) else {
            return Launcher::None;
        };
        let user_path = user_dir.join(&file);
        let ours = std::fs::read_to_string(&user_path)
            .map(|t| t.contains(LAUNCHER_MARK))
            .unwrap_or(false);

        if in_flatpak() {
            let base = PathBuf::from("/app/share/applications").join(&file);
            let Some((exec, try_exec)) = flatpak_exec() else { return Launcher::None };
            return Launcher::Shadow { base, user_path, ours, exec: Some(exec), try_exec, flatpak: true };
        }
        if user_path.exists() && !ours {
            return Launcher::Owned(user_path);
        }
        let dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        for dir in dirs.split(':') {
            let base = PathBuf::from(dir).join("applications").join(&file);
            if !base.is_file() {
                continue;
            }
            // Hide the copy once the packaged binary is gone: the first
            // word of the installed Exec, as the desktop resolves it.
            let try_exec = std::fs::read_to_string(&base)
                .ok()
                .and_then(|t| {
                    desktop_value(&t, "Exec")
                        .map(|e| e.split_whitespace().next().unwrap_or("").to_string())
                })
                .unwrap_or_default();
            return Launcher::Shadow { base, user_path, ours, exec: None, try_exec, flatpak: false };
        }
        Launcher::None
    }

    /// Point the launcher at `icon` (an absolute path); `restore` instead
    /// puts the installed launcher back (the default, where a copy exists).
    fn point_at(&self, icon: &str, restore: bool) {
        match self {
            Launcher::Shadow { base, user_path, ours, exec, try_exec, flatpak } => {
                write_shadow(base, user_path, icon, restore, *ours, exec.as_deref(), try_exec, *flatpak)
            }
            Launcher::Owned(path) => edit_icon_line(path, icon),
            Launcher::None => {}
        }
    }
}

/// The `Exec` line Flatpak exports for this app (from `/.flatpak-info`), and
/// the exported `bin` wrapper that exists exactly as long as it is installed.
fn flatpak_exec() -> Option<(String, String)> {
    let info = std::fs::read_to_string("/.flatpak-info").ok()?;
    let get = |key: &str| -> Option<String> {
        info.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .map(|v| v.trim().to_string())
    };
    let branch = get("branch")?;
    let arch = get("arch")?;
    let app_path = get("app-path")?;
    // …/flatpak/app/<id>/<arch>/<branch>/<commit>/files → …/flatpak
    let root = app_path
        .split_once(&format!("/app/{}/", crate::APP_ID))
        .map(|(root, _)| root.to_string())?;
    // `%U` as in the shipped entry: one launch for every selected file, so
    // "Send with Hylki" from Files attaches them all to the same message.
    let exec = format!(
        "flatpak run --branch={branch} --arch={arch} --command=hylki --file-forwarding {} @@u %U @@",
        crate::APP_ID
    );
    Some((exec, format!("{root}/exports/bin/{}", crate::APP_ID)))
}

/// One key's value from a desktop file's main group.
fn desktop_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .map(|v| v.trim().to_string())
}

#[allow(clippy::too_many_arguments)]
fn write_shadow(
    base: &std::path::Path,
    user_path: &std::path::Path,
    icon: &str,
    restore: bool,
    ours: bool,
    exec: Option<&str>,
    try_exec: &str,
    flatpak: bool,
) {
    if restore {
        if ours {
            match std::fs::remove_file(user_path) {
                Ok(()) => {
                    tracing::info!("removed {}", user_path.display());
                    note_launcher_written();
                }
                Err(e) => tracing::warn!("could not remove {}: {e}", user_path.display()),
            }
        }
        return;
    }
    let Ok(text) = std::fs::read_to_string(base) else {
        tracing::warn!("no launcher at {}", base.display());
        return;
    };
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with("Icon=") {
            out.push_str(&format!("Icon={icon}\n"));
        } else if line.starts_with("Exec=") && exec.is_some() {
            out.push_str(&format!("Exec={}\n", exec.unwrap_or("")));
        } else if line.starts_with("TryExec=")
            || line.starts_with("X-Flatpak=")
            || line.starts_with(LAUNCHER_MARK)
        {
            // Re-added below.
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !try_exec.is_empty() {
        out.push_str(&format!("TryExec={try_exec}\n"));
    }
    if flatpak {
        out.push_str(&format!("X-Flatpak={}\n", crate::APP_ID));
    }
    out.push_str(&format!("{LAUNCHER_MARK}=1\n"));
    if std::fs::read_to_string(user_path).map(|cur| cur == out).unwrap_or(false) {
        return;
    }
    if let Some(dir) = user_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(user_path, out) {
        Ok(()) => {
            tracing::info!("wrote {}", user_path.display());
            note_launcher_written();
        }
        Err(e) => tracing::warn!("could not write {}: {e}", user_path.display()),
    }
}

/// Replace the `Icon=` line of a launcher the user's own install owns.
fn edit_icon_line(path: &std::path::Path, icon: &str) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let mut out = String::new();
    let mut seen = false;
    for line in text.lines() {
        if line.starts_with("Icon=") {
            out.push_str(&format!("Icon={icon}\n"));
            seen = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !seen || out == text {
        return;
    }
    match std::fs::write(path, out) {
        Ok(()) => note_launcher_written(),
        Err(e) => tracing::warn!("could not update {}: {e}", path.display()),
    }
}

/// A texture of an icon, decoded at `px` pixels (call with 2x the display
/// size for HiDPI) rather than the full 512, to keep a gallery cheap.
pub fn texture(id: &str, px: i32) -> Option<gtk::gdk::Texture> {
    use gtk::gdk_pixbuf::PixbufLoader;
    use gtk::prelude::PixbufLoaderExt;
    thread_local! {
        // Decoded once per (icon, size): the gallery is rebuilt with every
        // Settings window, and decoding the catalogue is the slow part.
        static DECODED: std::cell::RefCell<std::collections::HashMap<(String, i32), gtk::gdk::Texture>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let key = (id.to_string(), px);
    if let Some(t) = DECODED.with(|c| c.borrow().get(&key).cloned()) {
        return Some(t);
    }
    let loader = PixbufLoader::with_type("png").ok()?;
    loader.set_size(px, px);
    loader.write(png_for(id)).ok()?;
    loader.close().ok()?;
    let texture = gtk::gdk::Texture::for_pixbuf(&loader.pixbuf()?);
    DECODED.with(|c| c.borrow_mut().insert(key, texture.clone()));
    Some(texture)
}

// ---------------------------------------------------------------------------
// Restarting
// ---------------------------------------------------------------------------

/// Command-line flag for the restart helper: waits for the running instance
/// to let go of its D-Bus name, then becomes a fresh one.
pub const RESTART_FLAG: &str = "--restart-helper";

/// Well-known name of the restart helper's D-Bus service. Inside Flatpak the
/// helper can't just be spawned — the sandbox is torn down with the process
/// that started it — so it is *activated*: the bus starts the exported
/// service (a new sandbox instance) and the running app exits under it.
pub fn restart_service_name() -> String {
    format!("{}.Restart", crate::APP_ID)
}

/// Start the helper that will bring the app back, and report whether it is
/// in place. The caller quits only on `Ok`, so a failed attempt leaves the
/// app running rather than gone.
pub fn launch_restart_helper() -> Result<(), String> {
    if in_flatpak() {
        activate_restart_service()
    } else {
        spawn_restart_helper()
    }
}

/// Outside Flatpak: run our own binary as a detached helper.
fn spawn_restart_helper() -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg(RESTART_FLAG)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Inside Flatpak: ask the session bus to start the helper service, which
/// the export turned into `flatpak run … --restart-helper`. Returns once
/// the helper owns its name (or the bus gave up).
fn activate_restart_service() -> Result<(), String> {
    use gtk::glib::prelude::ToVariant;
    let conn = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let reply = conn
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "StartServiceByName",
            Some(&(restart_service_name(), 0u32).to_variant()),
            None,
            gtk::gio::DBusCallFlags::NONE,
            15_000,
            gtk::gio::Cancellable::NONE,
        )
        .map_err(|e| e.to_string())?;
    match reply.get::<(u32,)>() {
        // 1 = started, 2 = was already running: either way a helper is up.
        Some((1,)) | Some((2,)) => Ok(()),
        other => Err(format!("unexpected StartServiceByName reply {other:?}")),
    }
}

/// The helper's whole life: own the service name (so a bus activation
/// completes), wait for the app to release its own name, then exec a normal
/// instance in this process — inside Flatpak that keeps the helper's
/// sandbox alive as the app's.
pub fn run_restart_helper() -> ! {
    use gtk::glib::prelude::ToVariant;
    use std::os::unix::process::CommandExt;
    let conn = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE).ok();
    if let Some(conn) = &conn {
        let _ = conn.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(restart_service_name(), 4u32).to_variant()), // DO_NOT_QUEUE
            None,
            gtk::gio::DBusCallFlags::NONE,
            3000,
            gtk::gio::Cancellable::NONE,
        );
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    // Give the app a moment to start quitting before the first look.
    std::thread::sleep(std::time::Duration::from_millis(300));
    while crate::primary_instance_running() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    drop(conn);
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("hylki"));
    // A one-off launch's review and capture switches must not carry over
    // into the instance that comes back.
    let err = std::process::Command::new(exe)
        .env_remove("HYLKI_WELCOME")
        .env_remove("HYLKI_SHOWCASE")
        .env_remove("HYLKI_SHOWCASE_PAGE")
        .env_remove("HYLKI_SHOWCASE_SETTINGS")
        .env_remove("HYLKI_SHOWCASE_SCROLL")
        .env_remove("HYLKI_SHOWCASE_DELAY")
        .exec();
    eprintln!("hylki: restart failed: {err}");
    std::process::exit(1);
}

/// The white envelope from the icon gallery as the image a dragged message
/// travels under: a cursor-sized picture whose texture holds `scale` device
/// pixels per logical one, so it stays crisp on a HiDPI display (a plain
/// 32px texture drawn there was upscaled and fuzzy). One texture per scale
/// is decoded, then shared.
pub fn drag_envelope(scale: i32) -> Option<gtk::Picture> {
    thread_local! {
        static TEXTURES: std::cell::RefCell<std::collections::HashMap<i32, gtk::gdk::Texture>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    use gtk::gdk_pixbuf::prelude::PixbufLoaderExt;
    use gtk::prelude::WidgetExt;
    let scale = scale.max(1);
    let texture = TEXTURES.with(|t| {
        if let Some(tex) = t.borrow().get(&scale) {
            return Some(tex.clone());
        }
        let png = CATALOG.iter().find(|c| c.id == "envelope-white")?.png;
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(png).ok()?;
        loader.close().ok()?;
        let pixbuf = loader.pixbuf()?;
        let px = DRAG_ICON_SIZE * scale;
        let scaled = pixbuf.scale_simple(px, px, gtk::gdk_pixbuf::InterpType::Hyper)?;
        let tex = gtk::gdk::Texture::for_pixbuf(&scaled);
        t.borrow_mut().insert(scale, tex.clone());
        Some(tex)
    })?;
    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_size_request(DRAG_ICON_SIZE, DRAG_ICON_SIZE);
    Some(picture)
}

/// The dragged-message envelope's edge, in logical px.
pub const DRAG_ICON_SIZE: i32 = 16;
