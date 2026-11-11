//! Hylki's settings: the files under `~/.config/hylki/` (accounts, the
//! preferences in privacy.toml, filters, tags, sidebar and window state, and
//! smaller ones), the secrets kept in the system keyring, and where the
//! config, cache and data directories are.
//!
//! Each file has its own child module; every public item is re-exported
//! here, so callers name them as `config::Item`.

use std::path::PathBuf;

mod accounts;
mod assistant;
mod bundle;
mod filters;
mod focus;
mod forwarded;
mod launch_files;
mod look;
mod prefs;
mod privacy;
mod remembered;
mod secrets;
mod sections;
mod smart_folders;
mod snoozed;
mod sound;
mod state;
mod tags;
mod toolbar;
mod window;

pub(crate) use accounts::*;
pub(crate) use assistant::*;
pub(crate) use bundle::*;
pub(crate) use filters::*;
pub(crate) use focus::*;
pub(crate) use forwarded::*;
pub(crate) use launch_files::*;
pub(crate) use look::*;
pub(crate) use prefs::*;
pub(crate) use privacy::*;
pub(crate) use remembered::*;
pub(crate) use secrets::*;
pub(crate) use sections::*;
pub(crate) use smart_folders::*;
pub(crate) use snoozed::*;
pub(crate) use sound::*;
pub(crate) use state::*;
pub(crate) use tags::*;
pub(crate) use toolbar::*;
pub(crate) use window::*;

#[cfg(test)]
mod filter_tests;
#[cfg(test)]
pub(crate) mod golden;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod tests;

/// Write a config file that only its owner can read, without ever having
/// existed as anything else.
///
/// `fs::write` then `set_permissions` leaves a window in which the file sits on
/// disk with whatever the umask allowed — brief, but these files carry
/// hostnames, usernames, OAuth client secrets and correspondent lists. Creating
/// with the mode already set closes it.
pub(crate) fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    write_private_bytes(path, contents.as_bytes())
}

/// [`write_private`] for bytes: the remote-content cache.
pub(crate) fn write_private_bytes(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // An existing file keeps its old mode through `OpenOptions::mode`, which
    // only applies at creation, so it is tightened before anything goes in.
    // Best effort: a document portal path may not take a mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    f.write_all(contents)
}

/// Create `dir`, and any parent missing, for its owner alone, and tighten
/// it if it already exists with more. A directory that refuses the mode stays
/// usable: mail that cannot be kept would be worse than mail kept as it was.
pub(crate) fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if std::fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
            if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
                tracing::warn!("could not make {} private: {e}", dir.display());
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// XDG base directories, with one twist: the beta channel running inside its
/// own Flatpak sandbox (app ID co.hyprlab.Hylki.Beta) would get an empty
/// `~/.var/app/co.hyprlab.Hylki.Beta` tree of its own — so it redirects to the
/// STABLE app's tree instead, sharing accounts, settings and the mail cache
/// with the stable install. The keyring service name is identical across
/// channels, so credentials are shared through the same redirection-free path.
/// Outside Flatpak (host builds) both channels already share `~/.config/hylki`
/// et al., so no redirection is needed or done.
fn shared_base(own: fn() -> Option<PathBuf>, sub: &str) -> Option<PathBuf> {
    if cfg!(feature = "beta")
        && std::env::var("FLATPAK_ID").is_ok_and(|id| id == "co.hyprlab.Hylki.Beta")
        && stable_data_present()
    {
        return Some(
            dirs::home_dir()?
                .join(".var/app/co.hyprlab.Hylki")
                .join(sub),
        );
    }
    own()
}

/// Whether the shared flatpak directory is actually reachable. Flatpak
/// silently SKIPS a `--filesystem` grant whose host path doesn't exist, which
/// left `~/.var/app/co.hyprlab.Hylki` pointing at the sandbox's throwaway
/// tmpfs on beta-only installs — accounts "saved" there and vanished on quit
/// (issue #83). The real fix is the manifest's `:create` suffix, which makes
/// flatpak create the host directory itself — a beta-first install thereby
/// establishes the standard persistent home a later stable install picks up,
/// in either install order. This check remains as defence in depth: should
/// the mount ever be missing anyway (an old manifest, a stripped-down
/// installation), the beta falls back to its own persistent home rather than
/// writing into the tmpfs. Decided once at first use, so our own writes
/// creating the path on the tmpfs mid-session can't flip it.
fn stable_data_present() -> bool {
    use std::sync::OnceLock;
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        dirs::home_dir()
            .map(|h| h.join(".var/app/co.hyprlab.Hylki").is_dir())
            .unwrap_or(false)
    })
}

pub(crate) fn config_base() -> Option<PathBuf> {
    shared_base(dirs::config_dir, "config")
}

pub(crate) fn cache_base() -> Option<PathBuf> {
    shared_base(dirs::cache_dir, "cache")
}

pub(crate) fn data_base() -> Option<PathBuf> {
    shared_base(dirs::data_dir, "data")
}

/// Write a settings file readable by the owner only (for modules outside
/// this one that keep their own file, like `cloud.toml`).
pub(crate) fn write_private_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    write_private(path, contents)
}

/// A file in Hylki's own config directory, `~/.config/hylki/`.
pub(crate) fn config_file(name: &str) -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join(name))
}

/// A settings file parsed, or `None` when it is missing, unreadable or does
/// not parse.
pub(crate) fn read_toml<T: serde::de::DeserializeOwned>(path: Option<PathBuf>) -> Option<T> {
    let text = std::fs::read_to_string(path?).ok()?;
    toml::from_str(&text).ok()
}

/// Writes a settings file only its owner can read, logging a failure as
/// "could not save `what`". `make_dir` creates the config directory first;
/// filters.toml, smart_folders.toml, tags.toml and cloud.toml are written
/// without it.
pub(crate) fn save_private_toml<T: serde::Serialize>(
    path: Option<PathBuf>,
    value: &T,
    what: &str,
    make_dir: bool,
) {
    let Some(path) = path else { return };
    if make_dir {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    match toml::to_string_pretty(value) {
        Ok(toml) => {
            if let Err(e) = replace_file(&path, |tmp| write_private(tmp, &toml)) {
                tracing::warn!("could not save {what}: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize {what}: {e}"),
    }
}

/// Write `path` through a file beside it that is then renamed over it, so
/// the file is never seen half written (#642). Written in place, a logout
/// that killed Hylki mid-write, or another thread reading at that moment,
/// found it empty: every remembered setting fell back to its default, and
/// the next save wrote the defaults back. A link is followed, so a config
/// directory kept as links into a dotfiles tree stays that way.
pub(crate) fn replace_file(
    path: &std::path::Path,
    write: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Unique per writer: two threads may save the same file at once.
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let done = write(&tmp).and_then(|()| std::fs::rename(&tmp, &path));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Writes a settings file with the mode the umask gives, quietly: these
/// files (window size, pane sizes, toolbar layout and the like) are best
/// effort.
fn save_plain_toml<T: serde::Serialize>(path: Option<PathBuf>, value: &T) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(toml) = toml::to_string_pretty(value) {
        let _ = replace_file(&path, |tmp| std::fs::write(tmp, &toml));
    }
}

// serde helpers shared by the files' structs.

fn is_false(b: &bool) -> bool {
    !*b
}

/// The default of the switches that start on.
fn default_on() -> bool {
    true
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_true(b: &bool) -> bool {
    *b
}
