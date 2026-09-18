//! Carrying an earlier install across.
//!
//! The app was Veem through 1.5.1 and Vireo through 1.34.0. A Hylki install
//! finds whatever those left behind and brings it over on its first start
//! — accounts, settings, the mail cache, credentials, the GNOME Files
//! extension — without changing or removing anything of theirs, so the
//! old app keeps working until the user removes it.
//!
//! Three shapes of predecessor data:
//!
//! * **Under the same base** (`~/.config/vireo` beside `~/.config/hylki`):
//!   a native install, or a Flatpak that `flatpak update` rebased from the
//!   old ID (Flatpak renames `~/.var/app/co.hyprlab.Vireo` to the new ID
//!   and leaves the inner `config/vireo` as it was). Renamed in place.
//! * **In the old app's own sandbox** (`~/.var/app/co.hyprlab.Vireo`),
//!   which the manifest mounts read-only: copied.
//! * **Keyring entries** under the old service name: copied lazily as each
//!   is first needed (`config::load_key`).

use std::path::{Path, PathBuf};

/// A name this app went by.
#[derive(Debug)]
pub struct Predecessor {
    /// Its Flatpak / desktop ID.
    pub app_id: &'static str,
    /// The subdirectory it used under the XDG bases.
    pub dir: &'static str,
    /// Its Secret Service service name.
    pub keyring_service: &'static str,
    /// Its GNOME Files extension file, installed on the host.
    pub nautilus_file: &'static str,
    /// The marker key in a launcher copy it wrote (see `app_icon`).
    pub launcher_mark: &'static str,
    /// What to call it.
    pub name: &'static str,
}

/// Newest first, so the most recent data wins when more than one is found.
pub const PREDECESSORS: &[Predecessor] = &[
    Predecessor {
        app_id: "co.hyprlab.Vireo",
        dir: "vireo",
        keyring_service: "co.hyprlab.Vireo",
        nautilus_file: "vireo-nautilus.py",
        launcher_mark: "X-Vireo-Icon-Launcher",
        name: "Vireo",
    },
    Predecessor {
        app_id: "com.getveem.Veem",
        dir: "veem",
        keyring_service: "com.getveem.Veem",
        nautilus_file: "veem-nautilus.py",
        launcher_mark: "X-Veem-Icon-Launcher",
        name: "Veem",
    },
];

/// Which predecessor this start brought data across from, if any — set
/// once by [`migrate_dirs`], read by the first-run notice.
static MIGRATED_FROM: std::sync::OnceLock<Option<&'static Predecessor>> = std::sync::OnceLock::new();

pub fn migrated_from() -> Option<&'static Predecessor> {
    MIGRATED_FROM.get().copied().flatten()
}

/// The XDG bases the app keeps state under, with the subdirectory of the
/// old app's sandbox each corresponds to.
fn bases() -> Vec<(&'static str, PathBuf)> {
    [
        ("config", crate::config::config_base()),
        ("cache", crate::config::cache_base()),
        ("data", crate::config::data_base()),
    ]
    .into_iter()
    .filter_map(|(sub, base)| base.map(|b| (sub, b)))
    .collect()
}

/// Bring an earlier install's directories across, once: for each base, the
/// first predecessor with data there fills in `<base>/hylki` when it does
/// not exist yet. Runs before anything reads its settings.
pub fn migrate_dirs() {
    let mut from: Option<&'static Predecessor> = None;
    let in_flatpak = Path::new("/.flatpak-info").exists();
    let home = dirs::home_dir();
    for (sub, base) in bases() {
        let new = base.join("hylki");
        if new.exists() {
            continue;
        }
        for pred in PREDECESSORS {
            // Same base: a native install, or a rebased Flatpak.
            let old = base.join(pred.dir);
            if old.is_dir() {
                match std::fs::rename(&old, &new) {
                    Ok(()) => {
                        tracing::info!("carried {} over to {}", old.display(), new.display());
                        from.get_or_insert(pred);
                    }
                    Err(e) => tracing::warn!("could not carry {} over: {e}", old.display()),
                }
                break;
            }
            // The old app's own sandbox, mounted read-only: copy.
            if !in_flatpak {
                continue;
            }
            let Some(home) = home.as_ref() else { continue };
            let old = home.join(".var/app").join(pred.app_id).join(sub).join(pred.dir);
            if !old.is_dir() || same_tree(&old, &new) {
                continue;
            }
            match copy_dir(&old, &new) {
                Ok(()) => {
                    tracing::info!("carried {} over to {}", old.display(), new.display());
                    from.get_or_insert(pred);
                }
                Err(e) => {
                    tracing::warn!("could not carry {} over: {e}", old.display());
                    // A half copy would stop the next start from trying again
                    // — and from finding the rest.
                    let _ = std::fs::remove_dir_all(&new);
                }
            }
            break;
        }
    }
    let _ = MIGRATED_FROM.set(from);
}

/// Whether `old` is, through symlinks, the very place `new` would be
/// (Flatpak leaves `~/.var/app/<old id>` pointing at the new one after a
/// rebase): copying a tree into itself is not a migration.
fn same_tree(old: &Path, new: &Path) -> bool {
    let Ok(old) = old.canonicalize() else { return false };
    let Some(parent) = new.parent() else { return false };
    let Ok(parent) = parent.canonicalize() else { return false };
    old.starts_with(&parent) || parent.starts_with(&old)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            // Reflinks where the filesystem has them (btrfs, XFS): the mail
            // cache then costs nothing until one side changes.
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// The user's home as the host sees it (inside Flatpak too).
fn host_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir)
}

/// Whether a predecessor is still installed, as far as this process can
/// see: its user-scope Flatpak export, or a binary on the usual paths. A
/// system-scope Flatpak is not visible from inside the sandbox, so
/// "not seen" is not "gone" — callers only act on what they can verify.
fn predecessor_seen(pred: &Predecessor) -> bool {
    let mut paths = vec![PathBuf::from("/var/lib/flatpak/exports/bin").join(pred.app_id)];
    if let Some(home) = host_home() {
        paths.push(home.join(".local/share/flatpak/exports/bin").join(pred.app_id));
        paths.push(home.join(".local/bin").join(pred.dir));
    }
    paths.push(PathBuf::from("/usr/bin").join(pred.dir));
    paths.push(PathBuf::from("/usr/local/bin").join(pred.dir));
    paths.iter().any(|p| p.exists())
}

/// Tidy what an earlier install left on the host, where it is safe to:
///
/// * Its GNOME Files extension is replaced by ours (the new file serves
///   the new app; two files would give Files two "Send with…" entries).
/// * Its per-user launcher copy and icon files (Settings → App icon) go
///   once its launch target is gone from a place this process can see,
///   so a predecessor still installed keeps the icon it was given.
pub fn tidy_host() {
    tidy_nautilus_extension();
    tidy_launchers();
}

fn tidy_nautilus_extension() {
    let Some(dir) = crate::nautilus_ext::dir() else { return };
    for pred in PREDECESSORS {
        let old = dir.join(pred.nautilus_file);
        if !old.exists() {
            continue;
        }
        let ours = dir.join(crate::nautilus_ext::FILE_NAME);
        if !ours.exists() {
            if let Err(e) = std::fs::write(&ours, crate::nautilus_ext::SOURCE) {
                tracing::warn!("could not install the Files extension in place of {}: {e}", old.display());
                continue;
            }
            tracing::info!("Files extension installed at {} in place of {}", ours.display(), old.display());
        }
        let _ = std::fs::remove_file(dir.join(format!(".{}.loaded", pred.nautilus_file.trim_end_matches(".py"))));
        match std::fs::remove_file(&old) {
            Ok(()) => tracing::info!("removed {}", old.display()),
            Err(e) => tracing::warn!("could not remove {}: {e}", old.display()),
        }
    }
}

fn tidy_launchers() {
    let Some(home) = host_home() else { return };
    let share = home.join(".local/share");
    for pred in PREDECESSORS {
        if predecessor_seen(pred) {
            continue;
        }
        let launcher = share.join("applications").join(format!("{}.desktop", pred.app_id));
        let ours = std::fs::read_to_string(&launcher)
            .map(|t| t.lines().any(|l| l.starts_with(pred.launcher_mark)))
            .unwrap_or(false);
        if !ours {
            continue;
        }
        // The launcher names its launch target; only act when that is gone.
        let try_exec = std::fs::read_to_string(&launcher)
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("TryExec=").map(str::to_string)));
        if try_exec.as_deref().is_some_and(|p| Path::new(p).exists()) {
            continue;
        }
        match std::fs::remove_file(&launcher) {
            Ok(()) => tracing::info!("removed {}", launcher.display()),
            Err(e) => tracing::warn!("could not remove {}: {e}", launcher.display()),
        }
        let prefix = format!("{}-", pred.app_id);
        for size in ["512x512", "256x256"] {
            let dir = share.join("icons/hicolor").join(size).join("apps");
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(&prefix) && name.ends_with(".png") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// The predecessor the desktop still opens `mailto:` links with, if any:
/// read from the host's `mimeapps.list`. The sandbox cannot change that
/// setting, so the user is pointed at Settings → Default Apps instead.
pub fn default_mailer() -> Option<&'static Predecessor> {
    let mut files = Vec::new();
    if let Some(home) = host_home() {
        files.push(home.join(".config/mimeapps.list"));
        files.push(home.join(".local/share/applications/mimeapps.list"));
    }
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let mut in_defaults = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_defaults = line == "[Default Applications]";
                continue;
            }
            if !in_defaults {
                continue;
            }
            if let Some(value) = line.strip_prefix("x-scheme-handler/mailto=") {
                let first = value.split(';').next().unwrap_or("").trim();
                return PREDECESSORS.iter().find(|p| first == format!("{}.desktop", p.app_id));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_tree_sees_through_a_symlink() {
        let root = std::env::temp_dir().join(format!("hylki-legacy-test-{}", std::process::id()));
        let new_base = root.join("co.hyprlab.Hylki/config");
        std::fs::create_dir_all(&new_base).unwrap();
        std::os::unix::fs::symlink(root.join("co.hyprlab.Hylki"), root.join("co.hyprlab.Vireo")).unwrap();
        let old = root.join("co.hyprlab.Vireo/config/vireo");
        std::fs::create_dir_all(&old).unwrap();
        assert!(same_tree(&old, &new_base.join("hylki")));
        let elsewhere = root.join("other/config/vireo");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert!(!same_tree(&elsewhere, &new_base.join("hylki")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn predecessors_are_newest_first() {
        assert_eq!(PREDECESSORS[0].app_id, "co.hyprlab.Vireo");
        assert_eq!(PREDECESSORS[1].app_id, "com.getveem.Veem");
    }
}
