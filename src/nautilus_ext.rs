//! The GNOME Files (Nautilus) right-click extension (#188): "Send with
//! Hylki" on selected files opens a composer with them attached.
//!
//! Nautilus loads its extensions on the host, outside any sandbox, so the
//! copy bundled here is installed into the user's own extension directory
//! on request (Settings → System → GNOME Files) and removed the same way.
//! The extension itself launches Hylki by desktop id, so it does not care
//! whether the app is the Flatpak or a native package.

use std::path::PathBuf;

/// The extension, verbatim: `data/nautilus/hylki-nautilus.py`.
pub const SOURCE: &str = include_str!("../data/nautilus/hylki-nautilus.py");
pub const FILE_NAME: &str = "hylki-nautilus.py";
/// Written by the extension itself when Files loads it: the SHA-256 of the
/// file it loaded (see `_mark_loaded` in the extension).
const LOADED_MARKER: &str = ".hylki-nautilus.loaded";

/// Whether (and which) copy of the extension the user's directory holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    /// The installed file is this build's copy.
    Installed,
    /// A file is installed but differs from this build's copy (an older
    /// release's, or edited by hand).
    Outdated,
}

/// The user's home — the host's, from inside the sandbox too.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir)
}

/// The per-user extension directory as Nautilus sees it. Inside Flatpak
/// XDG_DATA_HOME is the sandbox's private dir, so the host's default is
/// used (where the manifest mounts `xdg-data/nautilus-python/extensions`).
pub fn dir() -> Option<PathBuf> {
    let base = if crate::platform::is_flatpak() {
        home()?.join(".local/share")
    } else {
        dirs::data_dir()?
    };
    Some(base.join("nautilus-python/extensions"))
}

/// Where the extension is (or would be) installed.
pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join(FILE_NAME))
}

pub fn status() -> Status {
    path().map(|p| status_at(&p)).unwrap_or(Status::NotInstalled)
}

/// Everything the settings row shows, read in one go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub status: Status,
    /// Files has loaded this very copy (its marker carries our hash).
    pub loaded: bool,
    /// Whether the nautilus-python loader is on the system: `None` when that
    /// cannot be seen from here (inside the Flatpak the host's /usr is not
    /// mounted).
    pub loader: Option<bool>,
}

impl State {
    pub fn read() -> Self {
        let status = status();
        let loaded = status == Status::Installed
            && path().is_some_and(|p| loaded_at(&p.with_file_name(LOADED_MARKER)));
        Self { status, loaded, loader: loader_present() }
    }
}

fn source_hash() -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(SOURCE.as_bytes()))
}

fn loaded_at(marker: &std::path::Path) -> bool {
    std::fs::read_to_string(marker).is_ok_and(|text| text.trim() == source_hash())
}

/// Whether the nautilus-python bindings (the loader for Python extensions)
/// are installed, judged by their library in the places distributions put
/// it. `None` inside the Flatpak, which cannot see the host's /usr.
pub fn loader_present() -> Option<bool> {
    if crate::platform::is_flatpak() {
        return None;
    }
    const DIRS: &[&str] = &[
        "/usr/lib64/nautilus/extensions-4",
        "/usr/lib/nautilus/extensions-4",
        "/usr/lib/x86_64-linux-gnu/nautilus/extensions-4",
        "/usr/lib/aarch64-linux-gnu/nautilus/extensions-4",
        "/usr/local/lib64/nautilus/extensions-4",
        "/usr/local/lib/nautilus/extensions-4",
    ];
    Some(DIRS.iter().any(|d| {
        std::fs::read_dir(d).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                e.file_name().to_string_lossy().starts_with("libnautilus-python")
            })
        })
    }))
}

fn status_at(path: &std::path::Path) -> Status {
    match std::fs::read_to_string(path) {
        Err(_) => Status::NotInstalled,
        Ok(text) if text == SOURCE => Status::Installed,
        Ok(_) => Status::Outdated,
    }
}

/// Write (or overwrite) this build's copy of the extension.
pub fn install() -> Result<PathBuf, String> {
    let path = path().ok_or_else(|| "no home directory".to_string())?;
    install_at(&path)?;
    tracing::info!("nautilus extension installed at {}", path.display());
    Ok(path)
}

fn install_at(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, SOURCE).map_err(|e| format!("{}: {e}", path.display()))
}

/// Delete the installed copy, if any.
pub fn remove() -> Result<(), String> {
    let Some(path) = path() else { return Ok(()) };
    remove_at(&path)
}

fn remove_at(path: &std::path::Path) -> Result<(), String> {
    let _ = std::fs::remove_file(path.with_file_name(LOADED_MARKER));
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!("nautilus extension removed from {}", path.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Ask GNOME Files to quit (what `nautilus -q` does), so that its next
/// window opens with the extension loaded — or without it, once removed.
/// Nothing is started if Files is not running.
pub fn quit_files() -> Result<(), String> {
    use gtk::glib::prelude::ToVariant;
    let conn = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let params = (
        "quit",
        Vec::<gtk::glib::Variant>::new(),
        std::collections::HashMap::<String, gtk::glib::Variant>::new(),
    )
        .to_variant();
    match conn.call_sync(
        Some("org.gnome.Nautilus"),
        "/org/gnome/Nautilus",
        "org.gtk.Actions",
        "Activate",
        Some(&params),
        None,
        gtk::gio::DBusCallFlags::NO_AUTO_START,
        5000,
        gtk::gio::Cancellable::NONE,
    ) {
        Ok(_) => Ok(()),
        // Not running: nothing to restart, and nothing to report.
        Err(e) if e.matches(gtk::gio::DBusError::ServiceUnknown)
            || e.matches(gtk::gio::DBusError::NameHasNoOwner) =>
        {
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_status_remove_round_trip() {
        let dir = std::env::temp_dir().join(format!("hylki-nautilus-ext-{}", std::process::id()));
        let path = dir.join("extensions").join(FILE_NAME);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(status_at(&path), Status::NotInstalled);
        install_at(&path).unwrap();
        assert_eq!(status_at(&path), Status::Installed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SOURCE);
        // An older release's copy (anything that differs) reads as outdated.
        std::fs::write(&path, "# something else\n").unwrap();
        assert_eq!(status_at(&path), Status::Outdated);
        remove_at(&path).unwrap();
        assert_eq!(status_at(&path), Status::NotInstalled);
        // Removing what is not there is not an error.
        remove_at(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loaded_marker_must_carry_this_copys_hash() {
        let dir = std::env::temp_dir().join(format!("hylki-nautilus-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(LOADED_MARKER);
        assert!(!loaded_at(&marker));
        std::fs::write(&marker, "0000\n").unwrap();
        assert!(!loaded_at(&marker), "another release's load does not count");
        std::fs::write(&marker, format!("{}\n", source_hash())).unwrap();
        assert!(loaded_at(&marker));
        // Removing the extension takes the marker with it.
        let ext = dir.join(FILE_NAME);
        install_at(&ext).unwrap();
        remove_at(&ext).unwrap();
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_extension_names_both_builds() {
        assert!(SOURCE.contains("\"co.hyprlab.Hylki\""));
        assert!(SOURCE.contains("\"co.hyprlab.Hylki.Beta\""));
        assert!(SOURCE.contains("Nautilus.MenuProvider"));
    }
}
