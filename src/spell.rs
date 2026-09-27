//! Subject-line spell checking (#114): the same enchant engine WebKit uses
//! for the message body, reached directly — GTK entries have no checker of
//! their own, and libspelling isn't in the runtime.
//!
//! enchant is dlopen'd at runtime rather than linked: it is guaranteed
//! present wherever WebKitGTK is (the GNOME runtime included), but linking
//! at build time would demand its -devel package on every build machine.
//! If the library or the dictionary is missing, checking simply reports
//! nothing misspelled — same silence the body would show.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CString};
use std::rc::Rc;

extern "C" {
    // From glibc itself (libdl merged into libc since 2.34) — no crate, no
    // link flag needed beyond what every binary already has.
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type BrokerInit = unsafe extern "C" fn() -> *mut c_void;
type RequestDict = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type DictCheck = unsafe extern "C" fn(*mut c_void, *const c_char, isize) -> c_int;
type DictAdd = unsafe extern "C" fn(*mut c_void, *const c_char, isize);

pub struct SpellChecker {
    dict: *mut c_void,
    check: DictCheck,
    add: DictAdd,
}

impl SpellChecker {
    /// A checker for `lang`, or `None` when enchant or the dictionary is
    /// unavailable. The broker and dictionary are deliberately never freed:
    /// one per language per process, alive for its whole life.
    fn new(lang: &str) -> Option<SpellChecker> {
        unsafe {
            let so = CString::new("libenchant-2.so.2").ok()?;
            let lib = dlopen(so.as_ptr(), 1 /* RTLD_LAZY */);
            if lib.is_null() {
                return None;
            }
            let sym = |name: &str| {
                let n = CString::new(name).unwrap();
                dlsym(lib, n.as_ptr())
            };
            let init = sym("enchant_broker_init");
            let req = sym("enchant_broker_request_dict");
            let check = sym("enchant_dict_check");
            let add = sym("enchant_dict_add");
            if init.is_null() || req.is_null() || check.is_null() || add.is_null() {
                return None;
            }
            let init: BrokerInit = std::mem::transmute(init);
            let req: RequestDict = std::mem::transmute(req);
            let check: DictCheck = std::mem::transmute(check);
            let add: DictAdd = std::mem::transmute(add);
            let broker = init();
            if broker.is_null() {
                return None;
            }
            let tag = CString::new(lang).ok()?;
            let dict = req(broker, tag.as_ptr());
            if dict.is_null() {
                return None;
            }
            Some(SpellChecker { dict, check, add })
        }
    }

    /// Whether enchant knows no such word. Errors (and interior NULs) count
    /// as correctly spelled — a checker must never cry wolf.
    fn is_misspelled(&self, word: &str) -> bool {
        let Ok(w) = CString::new(word) else { return false };
        unsafe { (self.check)(self.dict, w.as_ptr(), word.len() as isize) > 0 }
    }

    /// Teach enchant a word: it joins the personal word list on disk (the
    /// same file WebKit's Learn Spelling writes) and stops being flagged by
    /// this checker at once.
    fn learn(&self, word: &str) {
        let Ok(w) = CString::new(word) else { return };
        unsafe { (self.add)(self.dict, w.as_ptr(), word.len() as isize) }
    }
}

thread_local! {
    /// The current checker, keyed by the language it was built for so a
    /// settings change swaps it.
    static CHECKER: RefCell<Option<(String, Option<Rc<SpellChecker>>)>> =
        const { RefCell::new(None) };
}

fn checker_for(lang: &str) -> Option<Rc<SpellChecker>> {
    CHECKER.with(|c| {
        let mut slot = c.borrow_mut();
        match slot.as_ref() {
            Some((l, found)) if l == lang => found.clone(),
            _ => {
                let built = SpellChecker::new(lang).map(Rc::new);
                *slot = Some((lang.to_string(), built.clone()));
                built
            }
        }
    })
}

/// Dictionary codes people actually encounter, named in the language itself
/// — a Russian speaker looking for their language finds "Русский", not a
/// code. Covers the runtime's English variants, the languages the Flatpak
/// bundles, and the common locale-extension arrivals.
const LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("de_AT", "Deutsch (Österreich)"),
    ("de_CH", "Deutsch (Schweiz)"),
    ("de_DE", "Deutsch (Deutschland)"),
    ("en_AG", "English (Antigua and Barbuda)"),
    ("en_AU", "English (Australia)"),
    ("en_BS", "English (Bahamas)"),
    ("en_BW", "English (Botswana)"),
    ("en_BZ", "English (Belize)"),
    ("en_CA", "English (Canada)"),
    ("en_DK", "English (Denmark)"),
    ("en_GB", "English (United Kingdom)"),
    ("en_GH", "English (Ghana)"),
    ("en_HK", "English (Hong Kong)"),
    ("en_IE", "English (Ireland)"),
    ("en_IN", "English (India)"),
    ("en_JM", "English (Jamaica)"),
    ("en_MW", "English (Malawi)"),
    ("en_NA", "English (Namibia)"),
    ("en_NG", "English (Nigeria)"),
    ("en_NZ", "English (New Zealand)"),
    ("en_PH", "English (Philippines)"),
    ("en_SG", "English (Singapore)"),
    ("en_TT", "English (Trinidad and Tobago)"),
    ("en_US", "English (United States)"),
    ("en_ZA", "English (South Africa)"),
    ("en_ZM", "English (Zambia)"),
    ("en_ZW", "English (Zimbabwe)"),
    ("es_AR", "Español (Argentina)"),
    ("es_ES", "Español (España)"),
    ("es_MX", "Español (México)"),
    ("fr_BE", "Français (Belgique)"),
    ("fr_CA", "Français (Canada)"),
    ("fr_CH", "Français (Suisse)"),
    ("fr_FR", "Français (France)"),
    ("it_IT", "Italiano (Italia)"),
    ("nl_BE", "Nederlands (België)"),
    ("nl_NL", "Nederlands (Nederland)"),
    ("pl_PL", "Polski (Polska)"),
    ("pt_BR", "Português (Brasil)"),
    ("pt_PT", "Português (Portugal)"),
    ("ru_RU", "Русский (Россия)"),
    ("sv_FI", "Svenska (Finland)"),
    ("sv_SE", "Svenska (Sverige)"),
    ("uk_UA", "Українська (Україна)"),
];

/// Language-only endonyms, for codes the exact table doesn't carry.
const LANGUAGE_FAMILY_NAMES: &[(&str, &str)] = &[
    ("af", "Afrikaans"),
    ("ar", "العربية"),
    ("be", "Беларуская"),
    ("bg", "Български"),
    ("bn", "বাংলা"),
    ("ca", "Català"),
    ("cs", "Čeština"),
    ("cy", "Cymraeg"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("el", "Ελληνικά"),
    ("en", "English"),
    ("es", "Español"),
    ("et", "Eesti"),
    ("eu", "Euskara"),
    ("fa", "فارسی"),
    ("fi", "Suomi"),
    ("fr", "Français"),
    ("ga", "Gaeilge"),
    ("gl", "Galego"),
    ("he", "עברית"),
    ("hi", "हिन्दी"),
    ("hr", "Hrvatski"),
    ("hu", "Magyar"),
    ("hy", "Հայերեն"),
    ("id", "Bahasa Indonesia"),
    ("is", "Íslenska"),
    ("it", "Italiano"),
    ("ka", "ქართული"),
    ("kk", "Қазақша"),
    ("ko", "한국어"),
    ("lt", "Lietuvių"),
    ("lv", "Latviešu"),
    ("mk", "Македонски"),
    ("nb", "Norsk bokmål"),
    ("nl", "Nederlands"),
    ("nn", "Norsk nynorsk"),
    ("no", "Norsk"),
    ("pl", "Polski"),
    ("pt", "Português"),
    ("ro", "Română"),
    ("ru", "Русский"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("sq", "Shqip"),
    ("sr", "Српски"),
    ("sv", "Svenska"),
    ("ta", "தமிழ்"),
    ("th", "ไทย"),
    ("tr", "Türkçe"),
    ("uk", "Українська"),
    ("uz", "Oʻzbekcha"),
    ("vi", "Tiếng Việt"),
];

/// A dictionary code as a human reads it: the language named in itself
/// ("Русский (Россия)" for ru_RU). Unknown regions fall back to the language
/// name plus the raw region; unknown languages to the raw code — never
/// nothing.
pub fn language_display_name(code: &str) -> String {
    if let Some((_, name)) = LANGUAGE_NAMES.iter().find(|(c, _)| *c == code) {
        return (*name).to_string();
    }
    let lang = code.split(['_', '-']).next().unwrap_or(code);
    if let Some((_, name)) = LANGUAGE_FAMILY_NAMES.iter().find(|(c, _)| *c == lang) {
        return match code.split(['_', '-']).nth(1) {
            Some(region) => format!("{name} ({region})"),
            None => (*name).to_string(),
        };
    }
    code.to_string()
}

/// Subject-line prefixes and mail shorthand no dictionary carries.
const MAIL_WORDS: &[&str] = &["Re", "RE", "re", "Fwd", "FWD", "Fw", "FW"];

/// The words in `text` worth checking, as (byte start, byte end, word).
/// Whitespace-separated tokens carrying an '@', a digit, or a scheme are
/// skipped whole — addresses, versions and links aren't prose — and within
/// the rest, maximal alphabetic runs (apostrophes included) of two letters
/// or more are the words.
fn checkable_words(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut token_start = 0;
    for token in text.split_whitespace() {
        let start = token_start + text[token_start..].find(token).unwrap_or(0);
        token_start = start + token.len();
        if token.contains('@') || token.contains("://") || token.chars().any(|c| c.is_ascii_digit())
        {
            continue;
        }
        let mut word_start: Option<usize> = None;
        // One past the end so a trailing word closes like an interior one.
        for (i, ch) in token.char_indices().chain(std::iter::once((token.len(), ' '))) {
            let wordish = ch.is_alphabetic() || ch == '\'' || ch == '\u{2019}';
            match (word_start, wordish) {
                (None, true) => word_start = Some(i),
                (Some(ws), false) => {
                    let w = &token[ws..i];
                    if w.chars().filter(|c| c.is_alphabetic()).count() >= 2
                        && !MAIL_WORDS.contains(&w)
                    {
                        out.push((start + ws, start + i, w));
                    }
                    word_start = None;
                }
                _ => {}
            }
        }
    }
    out
}

/// Whether one word is misspelled under the current preference and language.
/// `false` whenever checking is off or unavailable.
pub fn word_is_misspelled(word: &str) -> bool {
    if !crate::config::load_privacy().spellcheck {
        return false;
    }
    let lang = crate::ui::rich_editor::resolved_spell_language();
    checker_for(&lang).is_some_and(|c| c.is_misspelled(word))
}

/// Pango error-underline attributes for every misspelled word in `text` —
/// `None` (clear the entry's attributes) when checking is off or enchant is
/// unavailable. A word whose byte range contains `cursor` is left unmarked:
/// while the cursor sits in a word it is still being typed, and flagging a
/// half-word on every keystroke reads as nagging. Pass `None` (after a
/// typing pause) to check the cursor's word too.
pub fn error_attrs(text: &str, cursor: Option<usize>) -> Option<gtk::pango::AttrList> {
    if !crate::config::load_privacy().spellcheck {
        return None;
    }
    let lang = crate::ui::rich_editor::resolved_spell_language();
    let checker = checker_for(&lang)?;
    let attrs = gtk::pango::AttrList::new();
    for (start, end, word) in checkable_words(text) {
        if cursor.is_some_and(|c| c >= start && c <= end) {
            continue;
        }
        if checker.is_misspelled(word) {
            let mut a = gtk::pango::AttrInt::new_underline(gtk::pango::Underline::Error);
            a.set_start_index(start as u32);
            a.set_end_index(end as u32);
            attrs.insert(a);
            // The error underline takes the text color unless told
            // otherwise; misspellings are red (GNOME's @error_color).
            let mut c = gtk::pango::AttrColor::new_underline_color(0xe0e0, 0x1b1b, 0x2424);
            c.set_start_index(start as u32);
            c.set_end_index(end as u32);
            attrs.insert(c);
        }
    }
    Some(attrs)
}

/// The personal word list for the active language: the plain
/// one-word-per-line file enchant keeps in the user config dir — the same
/// list the body's "Learn Spelling" menu item feeds.
fn personal_dict_path() -> std::path::PathBuf {
    let lang = crate::ui::rich_editor::resolved_spell_language();
    gtk::glib::user_config_dir().join("enchant").join(format!("{lang}.dic"))
}

/// The words the user has taught the spell checker, for the active language.
pub fn personal_words() -> Vec<String> {
    std::fs::read_to_string(personal_dict_path())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Every language's personal word list in the enchant config dir, for a
/// settings backup: language → words.
pub fn all_personal_words() -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out = std::collections::BTreeMap::new();
    let dir = gtk::glib::user_config_dir().join("enchant");
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("dic") {
            continue;
        }
        let Some(lang) = path.file_stem().and_then(|x| x.to_str()) else { continue };
        let words: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        if !words.is_empty() {
            out.insert(lang.to_string(), words);
        }
    }
    out
}

/// Add a backup's words for `lang` to its personal list, keeping what is
/// already there (nothing is removed). The file is enchant's own
/// one-word-per-line list, created when absent.
pub fn merge_personal_words(lang: &str, words: &[String]) {
    if words.is_empty() || lang.is_empty() || lang.contains('/') || lang.contains("..") {
        return;
    }
    let dir = gtk::glib::user_config_dir().join("enchant");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{lang}.dic"));
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut have: std::collections::BTreeSet<String> =
        existing.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for w in words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
        if have.insert(w.to_string()) {
            out.push_str(w);
            out.push('\n');
        }
    }
    let _ = std::fs::write(&path, out);
    CHECKER.with(|c| *c.borrow_mut() = None);
}

/// Teach the checker a word: through enchant itself, so the file is written
/// the way its own tooling writes it, and the subject's checker accepts the
/// word immediately. The message body's checker (WebKit's own enchant
/// instance) picks it up at the next launch.
pub fn add_personal_word(word: &str) {
    let word = word.trim();
    if word.is_empty() {
        return;
    }
    let lang = crate::ui::rich_editor::resolved_spell_language();
    if let Some(c) = checker_for(&lang) {
        c.learn(word);
    }
}

/// Unlearn a word: enchant's own "remove" would blacklist it instead of
/// forgetting it, so the personal list is rewritten without the word and the
/// cached checker is dropped to reload the trimmed list. The message body
/// applies the change at the next launch.
pub fn remove_personal_word(word: &str) {
    let path = personal_dict_path();
    let Ok(content) = std::fs::read_to_string(&path) else { return };
    let kept: Vec<&str> =
        content.lines().filter(|l| l.trim() != word && !l.trim().is_empty()).collect();
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = std::fs::write(&path, out);
    CHECKER.with(|c| *c.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::checkable_words;

    #[test]
    fn words_are_found_and_noise_is_skipped() {
        let words: Vec<&str> =
            checkable_words("Re: meet ada@x.com at 3pm — v1.2 recieve https://x.y ok")
                .iter()
                .map(|(_, _, w)| *w)
                .collect();
        // "Re" is mail shorthand, the address/version/time/link tokens carry
        // digits or schemes, and single letters aren't words. "at" IS a word
        // — short, but the dictionary knows it, so it never underlines.
        assert_eq!(words, ["meet", "at", "recieve", "ok"]);
    }

    #[test]
    fn byte_ranges_point_at_the_words() {
        let text = "héllo wörld";
        for (s, e, w) in checkable_words(text) {
            assert_eq!(&text[s..e], w);
        }
        assert_eq!(checkable_words(text).len(), 2);
    }
}
