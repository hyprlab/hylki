//! The one-line settings files read before anything else starts: the
//! interface language, the GTK renderer and the mail cache's encryption.

use std::path::PathBuf;

use super::config_file;

/// The interface language the user chose in Settings, as a locale code
/// ("fr", "en"); empty = the system's own (#179). Its own small file
/// rather than a preference in privacy.toml: it is read before anything
/// else starts, when no TOML has been parsed yet.
pub(crate) fn load_language() -> String {
    read_line_file(language_path())
}

pub(crate) fn save_language(lang: &str) {
    write_line_file(language_path(), lang);
}

/// How GTK draws the windows (#403): empty for GTK's own choice, else a
/// `GSK_RENDERER` value ("vulkan", "gl", "cairo"). Its own small file, as
/// the language is: it has to be read before GTK starts.
pub(crate) fn load_renderer() -> String {
    read_line_file(renderer_path())
}

pub(crate) fn save_renderer(renderer: &str) {
    write_line_file(renderer_path(), renderer);
}

/// Whether the mail cache is encrypted (`cache::at_rest`): on unless
/// Settings → Privacy turned it off. Its own small file, as the renderer
/// is: the cache is unlocked before anything else opens it.
pub(crate) fn cache_encrypted() -> bool {
    read_line_file(config_file("cache-encryption")) != "off"
}

pub(crate) fn save_cache_encrypted(on: bool) {
    write_line_file(config_file("cache-encryption"), if on { "" } else { "off" });
}

fn renderer_path() -> Option<PathBuf> {
    config_file("renderer")
}

fn language_path() -> Option<PathBuf> {
    config_file("language")
}

/// The value in a one-line file, trimmed; empty when there is none.
fn read_line_file(path: Option<PathBuf>) -> String {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Writes `value`, trimmed, as the file's one line; a blank value removes
/// the file. Best effort: a failure leaves the file as it was.
fn write_line_file(path: Option<PathBuf>, value: &str) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if value.trim().is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = std::fs::write(&path, format!("{}\n", value.trim()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_file_holds_one_trimmed_line_or_nothing() {
        let dir = std::env::temp_dir().join(format!("hylki-line-file-{}", std::process::id()));
        let path = dir.join("sub").join("language");
        write_line_file(Some(path.clone()), "  fr \n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fr\n");
        assert_eq!(read_line_file(Some(path.clone())), "fr");
        write_line_file(Some(path.clone()), "  ");
        assert!(!path.exists());
        assert_eq!(read_line_file(Some(path.clone())), "");
        assert_eq!(read_line_file(None), "");
        let _ = std::fs::remove_dir_all(dir);
    }
}
