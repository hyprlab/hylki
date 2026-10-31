//! Grammar and style checking (#452): what is written in the composer is
//! checked by the checker the user chose, and the problems it finds come
//! back as places in that text. None of them ships with the app: Harper
//! (English) and LanguageTool are downloaded from Settings when asked for
//! and run on this computer, or a LanguageTool server the user runs or has
//! an account with is asked over the network.
//!
//! The settings live in `grammar.toml`; a LanguageTool Premium key, when
//! there is one, is in the keyring. Each paragraph is checked in the
//! language it is written in, told offline from among the languages chosen
//! for spelling, so a message written partly in French and partly in English
//! is checked as both.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::i18n::i18n;

pub(crate) mod harper;
pub(crate) mod install;
mod language;
mod languagetool;
pub(crate) mod local;

/// What checks the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Engine {
    /// Harper, downloaded: English only, on this computer.
    Harper,
    /// LanguageTool and Java, downloaded and run on this computer.
    Local,
    /// A LanguageTool server at an address.
    Server,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Settings {
    /// Check what is written as it is written.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// What checks it; unset in a file from before there was a choice,
    /// which had only a server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) engine: Option<Engine>,
    /// The server, e.g. `http://localhost:8081` or
    /// `https://api.languagetoolplus.com`.
    #[serde(default)]
    pub(crate) url: String,
    /// LanguageTool Premium: the account's email address, sent with the key.
    #[serde(default)]
    pub(crate) username: String,
}

impl Settings {
    /// The checker chosen, or for a file that names none, the server when
    /// it has an address and Harper when not.
    pub(crate) fn engine(&self) -> Engine {
        self.engine.unwrap_or(if self.url.trim().is_empty() {
            Engine::Harper
        } else {
            Engine::Server
        })
    }

    /// Whether there is anything to check with.
    pub(crate) fn active(&self) -> bool {
        self.enabled
            && match self.engine() {
                Engine::Harper => harper::installed().is_some(),
                Engine::Local => local::installed().is_some(),
                Engine::Server => languagetool::endpoint(&self.url).is_some(),
            }
    }
}

/// The checker to offer someone who has not chosen one: Harper when every
/// language they write in is English, LanguageTool when not.
pub(crate) fn suggested_engine(languages: &[String]) -> Engine {
    let english = languages
        .iter()
        .all(|l| l.to_ascii_lowercase().starts_with("en"));
    if english && harper::available() {
        Engine::Harper
    } else {
        Engine::Local
    }
}

/// One problem in the text: where it is, in UTF-16 code units as the
/// editor's script counts them (and as the server does), what is wrong,
/// and what could go there instead.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Issue {
    pub(crate) offset: usize,
    pub(crate) length: usize,
    pub(crate) message: String,
    pub(crate) replacements: Vec<String>,
    pub(crate) rule: String,
    /// A matter of style rather than a mistake, marked more quietly.
    pub(crate) style: bool,
}

/// The most suggestions a problem keeps: the menu shows them all, and the
/// server can offer dozens for a single word.
pub(super) const SUGGESTIONS: usize = 5;
/// The most characters sent in one request: under the 20,000 the public
/// server takes from an account without a key.
pub(super) const PIECE: usize = 15_000;
/// Past this nothing is checked: a long forwarded log pasted in would
/// take minutes and use up an account's allowance.
const LIMIT: usize = 100_000;

// ─── Settings file and key ────────────────────────────────────────────────

fn path() -> Option<PathBuf> {
    crate::config::config_file("grammar.toml")
}

pub(crate) fn load() -> Settings {
    crate::config::read_toml(path()).unwrap_or_default()
}

pub(crate) fn save(settings: &Settings) {
    let Some(path) = path() else { return };
    match toml::to_string_pretty(settings) {
        Ok(text) => {
            if let Err(e) = crate::config::write_private_file(&path, &text) {
                tracing::warn!("could not save the grammar settings: {e}");
            }
        }
        Err(e) => tracing::warn!("could not save the grammar settings: {e}"),
    }
}

const KEY_NAME: &str = "grammar:languagetool";

/// The key as last read from or written to the keyring, `None` until then,
/// so the composer reads the keyring once a session and not per check.
static KEY: Mutex<Option<String>> = Mutex::new(None);

/// The key, from the keyring (blocking: the keyring thread's job).
pub(crate) fn load_key() -> String {
    let key = crate::config::load_cloud_password(KEY_NAME)
        .map(|k| k.trim().to_string())
        .unwrap_or_default();
    if let Ok(mut k) = KEY.lock() {
        *k = Some(key.clone());
    }
    key
}

/// The key as already read, if it has been.
pub(crate) fn known_key() -> Option<String> {
    KEY.lock().ok()?.clone()
}

pub(crate) fn store_key(key: &str) {
    let key = key.trim();
    if key.is_empty() {
        crate::config::delete_cloud_password(KEY_NAME);
    } else if let Err(e) = crate::config::store_cloud_password(KEY_NAME, key) {
        tracing::warn!("could not store the LanguageTool key: {e}");
    }
    if let Ok(mut k) = KEY.lock() {
        *k = Some(key.to_string());
    }
}

// ─── Ignoring ─────────────────────────────────────────────────────────────

/// Problems the user said to leave alone, by rule and the text it was
/// found in, for the rest of the session and in every composer.
static IGNORED: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);

pub(crate) fn ignore(rule: &str, text: &str) {
    if let Ok(mut set) = IGNORED.lock() {
        set.get_or_insert_with(Default::default)
            .insert((rule.to_string(), text.to_string()));
    }
}

pub(super) fn ignored(rule: &str, text: &str) -> bool {
    IGNORED.lock().ok().is_some_and(|set| {
        set.as_ref()
            .is_some_and(|s| s.contains(&(rule.to_string(), text.to_string())))
    })
}

// ─── Checking ─────────────────────────────────────────────────────────────

/// Check `text` (blocking; call off the UI thread). `languages` are the
/// spelling languages ("fr_FR"); `spelling` says the spell checker is on,
/// which then keeps the misspelled words to itself.
pub(crate) fn check(
    settings: &Settings,
    key: &str,
    text: &str,
    languages: &[String],
    spelling: bool,
) -> Result<Vec<Issue>, String> {
    if text.chars().count() > LIMIT {
        return Ok(Vec::new());
    }
    match settings.engine() {
        Engine::Harper => harper::check(text, languages, spelling),
        Engine::Local => {
            let base = local::address()?;
            languagetool::check(
                &base,
                &languagetool::Account {
                    username: "",
                    key: "",
                },
                text,
                languages,
                spelling,
            )
        }
        Engine::Server => {
            let Some(base) = languagetool::endpoint(&settings.url) else {
                return Err(i18n(
                    "Enter the LanguageTool server's address in Settings → Composing.",
                ));
            };
            let account = languagetool::Account {
                username: settings.username.trim(),
                key,
            };
            languagetool::check(&base, &account, text, languages, spelling)
        }
    }
}

/// Ask the server in the settings whether it answers: its name and
/// version, or why not.
pub(crate) fn check_server(settings: &Settings, key: &str) -> Result<String, String> {
    let Some(base) = languagetool::endpoint(&settings.url) else {
        return Err(i18n("Enter the server's address first."));
    };
    let account = languagetool::Account {
        username: settings.username.trim(),
        key,
    };
    languagetool::check_server(&base, &account)
}

/// The part of `text` at a UTF-16 offset and length.
fn utf16_slice(text: &str, offset: usize, length: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().skip(offset).take(length).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_count_as_the_editor_does() {
        // "😀" is two UTF-16 units, "é" one.
        assert_eq!(utf16_slice("😀 é a test", 3, 1), "é");
        assert_eq!(utf16_slice("😀 é a test", 0, 2), "😀");
    }

    #[test]
    fn a_file_from_before_the_choice_keeps_its_server() {
        let old: Settings =
            toml::from_str("enabled = true\nurl = \"http://localhost:8010\"\n").unwrap();
        assert_eq!(old.engine(), Engine::Server);
        assert_eq!(Settings::default().engine(), Engine::Harper);
        let chosen: Settings = toml::from_str("engine = \"local\"\n").unwrap();
        assert_eq!(chosen.engine(), Engine::Local);
    }

    #[test]
    fn english_writers_are_offered_harper() {
        let langs = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        if harper::available() {
            assert_eq!(
                suggested_engine(&langs(&["en_US", "en_GB"])),
                Engine::Harper
            );
        }
        assert_eq!(suggested_engine(&langs(&["en_US", "fr_FR"])), Engine::Local);
    }
}

/// The whole path, from download to marks, against the real releases. Run
/// it with a scratch data folder, as it downloads about 320 MB:
/// `XDG_DATA_HOME=/tmp/gr-data cargo test -- --ignored live_downloads --nocapture`.
#[cfg(test)]
#[test]
#[ignore]
fn live_downloads_then_checks() {
    assert!(std::env::var("XDG_DATA_HOME").is_ok_and(|d| d.starts_with("/tmp")));
    let progress = install::Progress::default();
    harper::download(&progress).unwrap();
    assert_eq!(harper::installed().as_deref(), Some(harper::VERSION));
    let english = Settings {
        enabled: true,
        engine: Some(Engine::Harper),
        ..Default::default()
    };
    assert!(english.active());
    let text = "She go home after work.";
    let found = check(&english, "", text, &["en_US".into()], true).unwrap();
    assert_eq!(utf16_slice(text, found[0].offset, found[0].length), "go");

    local::download(&progress).unwrap();
    assert_eq!(local::installed().as_deref(), Some(local::VERSION));
    let every = Settings {
        engine: Some(Engine::Local),
        ..english
    };
    let text = "Je vais a la plage demain avec mes amis.";
    let started = std::time::Instant::now();
    let found = check(&every, "", text, &["fr_FR".into()], true).unwrap();
    eprintln!("first LanguageTool check took {:?}", started.elapsed());
    assert!(found
        .iter()
        .any(|i| utf16_slice(text, i.offset, i.length) == "a"));
    local::stop();
}
