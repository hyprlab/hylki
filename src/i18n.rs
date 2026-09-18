//! Translations, through gettext.
//!
//! Every user-facing string goes through [`i18n`] (or its plural and
//! placeholder variants), which looks it up in the catalogue for the
//! session's language and falls back to the English source string. The
//! catalogues are compiled from `po/<lang>.po` into `<localedir>/<lang>/
//! LC_MESSAGES/hylki.mo` — by the Flatpak build for installs, by
//! `tools/build-locale.sh` for a source-tree run. `tools/update-pot.sh`
//! regenerates the template (`po/hylki.pot`) from the source; translators
//! only ever touch `po/<lang>.po`.
//!
//! Placeholders are named: `i18n_f("Marked {n} messages", &[("n", &n)])`,
//! so a translation can reorder them. Plurals go through `ni18n`, which
//! picks the right form for the language rather than an English `s`.

use std::path::{Path, PathBuf};

use gettextrs::LocaleCategory;

const DOMAIN: &str = "hylki";

/// Bind the text domain. Called once, first thing in `main`, before GTK
/// (which would otherwise set the locale without our domain in place).
pub fn init() {
    // A language chosen in Settings (#179) goes to gettext through
    // LANGUAGE, which it consults on every lookup, ahead of the locale.
    // Under the C locale gettext ignores LANGUAGE, so a system without a
    // locale of its own still gets the chosen language via C.UTF-8 for
    // messages. Single-threaded here, so setting the environment is safe.
    let chosen = crate::config::load_language();
    apply_language(&chosen);
    // SAFETY: called once at the very start of main, before any other
    // thread exists (setlocale is not thread-safe).
    unsafe { gettextrs::setlocale(LocaleCategory::LcAll, "") };
    if !chosen.is_empty() {
        let bare = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty())
            .is_none_or(|v| v == "C" || v == "POSIX");
        if bare {
            unsafe { gettextrs::setlocale(LocaleCategory::LcMessages, "C.UTF-8") };
        }
    }
    let Some(dir) = locale_dir() else { return };
    let bound = gettextrs::bindtextdomain(DOMAIN, dir.clone())
        .and_then(|_| gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8"))
        .and_then(|_| gettextrs::textdomain(DOMAIN));
    match bound {
        Ok(_) => tracing::debug!("translations bound to {}", dir.display()),
        Err(e) => tracing::debug!("translations not bound: {e}"),
    }
}

/// What LANGUAGE was before this app set it, kept in the environment so a
/// restarted instance (which inherits the environment) can put it back:
/// otherwise a language once chosen followed the app through every
/// restart, even after the choice went back to the system's.
const LANGUAGE_ORIG: &str = "HYLKI_LANGUAGE_ORIG";

/// Point gettext at `code` ("" = the system's): restore the LANGUAGE the
/// session had, then set the choice over it.
pub fn apply_language(code: &str) {
    if let Some(orig) = std::env::var_os(LANGUAGE_ORIG) {
        if orig.is_empty() {
            std::env::remove_var("LANGUAGE");
        } else {
            std::env::set_var("LANGUAGE", orig);
        }
    } else {
        std::env::set_var(LANGUAGE_ORIG, std::env::var_os("LANGUAGE").unwrap_or_default());
    }
    if !code.is_empty() {
        std::env::set_var("LANGUAGE", code);
    }
}

/// Where the catalogues live: an override for testing, the source tree's
/// own build (tools/build-locale.sh) when running uninstalled, the install
/// prefix beside the binary (/app in Flatpak, /usr for a package), then the
/// usual system prefixes. The first directory holding any of our
/// catalogues wins, so a source-tree run never falls back to a stale
/// system copy — and vice versa.
fn locale_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("HYLKI_LOCALEDIR") {
        candidates.push(PathBuf::from(dir));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("po/.build"));
    let prefix_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().and_then(|p| p.parent()).map(|p| p.join("share/locale")));
    if let Some(p) = &prefix_dir {
        candidates.push(p.clone());
    }
    candidates.push(PathBuf::from("/usr/local/share/locale"));
    candidates.push(PathBuf::from("/usr/share/locale"));
    candidates
        .iter()
        .find(|dir| has_catalogue(dir))
        .cloned()
        .or(prefix_dir)
}

/// Whether `dir/<lang>/LC_MESSAGES/hylki.mo` exists for any language.
fn has_catalogue(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries
        .flatten()
        .any(|e| e.path().join("LC_MESSAGES").join(format!("{DOMAIN}.mo")).is_file())
}

/// The translation of `s`, or `s` itself.
pub fn i18n(s: &str) -> String {
    gettextrs::gettext(s)
}

/// The plural-aware translation: `singular` or `plural` in English, the
/// language's own form for `n` elsewhere.
pub fn ni18n(singular: &str, plural: &str, n: u32) -> String {
    gettextrs::ngettext(singular, plural, n)
}

/// Translate, then fill `{name}` placeholders from `args`.
pub fn i18n_f(s: &str, args: &[(&str, &str)]) -> String {
    fill(gettextrs::gettext(s), args)
}

/// Plural-aware [`i18n_f`]; `{n}` is not filled in automatically, pass it.
pub fn ni18n_f(singular: &str, plural: &str, n: u32, args: &[(&str, &str)]) -> String {
    fill(gettextrs::ngettext(singular, plural, n), args)
}

/// Mark a string for extraction without translating it here — for
/// tables of literals whose entries are translated where they are shown.
pub const fn i18n_noop(s: &'static str) -> &'static str {
    s
}

fn fill(mut s: String, args: &[(&str, &str)]) -> String {
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}
