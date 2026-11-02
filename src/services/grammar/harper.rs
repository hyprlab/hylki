//! Harper (#452): an English grammar checker that runs on this computer,
//! downloaded when the user asks for it. Its command-line program reads
//! the text on its input and answers in JSON, so it runs once per check
//! and nothing stays in memory between them.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;

use super::install::{self, Archive, Kind, Progress};
use super::language::{pieces, server_code};
use super::{Issue, PIECE, SUGGESTIONS};
use crate::i18n::{i18n, i18n_f};

pub(crate) const VERSION: &str = "3.0.0";

/// The program for this computer. The musl builds are static, so they run
/// on any distribution and inside the Flatpak's runtime alike.
fn archive() -> Option<&'static Archive> {
    const X86_64: Archive = Archive {
        url: "https://github.com/Automattic/harper/releases/download/v3.0.0/harper-cli-x86_64-unknown-linux-musl.tar.gz",
        size: 13_445_972,
        sha256: "fb15a5a2deed78ea68c02ef048b80680a7cd770cde1712bedfb42207410ac53a",
        kind: Kind::TarGz,
        folder: "bin",
    };
    const AARCH64: Archive = Archive {
        url: "https://github.com/Automattic/harper/releases/download/v3.0.0/harper-cli-aarch64-unknown-linux-musl.tar.gz",
        size: 13_008_288,
        sha256: "9a29e026662b219f31987101f0fdcaab7895804f6686bfe18fc6357dfff64e55",
        kind: Kind::TarGz,
        folder: "bin",
    };
    match std::env::consts::ARCH {
        "x86_64" => Some(&X86_64),
        "aarch64" => Some(&AARCH64),
        _ => None,
    }
}

/// Unpacked, the program is about 61 MB.
const SPACE: u64 = 80_000_000;

fn dir() -> Option<PathBuf> {
    install::folder("harper")
}

fn program() -> Option<PathBuf> {
    Some(dir()?.join("bin").join("harper-cli"))
}

/// Whether Harper can be downloaded for this computer at all.
pub(crate) fn available() -> bool {
    archive().is_some()
}

/// The version downloaded, if it is.
pub(crate) fn installed() -> Option<String> {
    let v = install::installed(&dir()?)?;
    program()?.is_file().then_some(v)
}

pub(crate) fn download_size() -> u64 {
    archive().map_or(0, |a| a.size)
}

/// Download and unpack Harper (blocking).
pub(crate) fn download(progress: &Progress) -> Result<(), String> {
    let archive = archive().ok_or_else(not_available)?;
    let dir = dir().ok_or_else(not_available)?;
    install::install(&dir, VERSION, &[archive], SPACE, progress)
}

pub(crate) fn remove() -> Result<(), String> {
    install::remove(&dir().ok_or_else(not_available)?)
}

fn not_available() -> String {
    i18n("Harper is not available for this computer.")
}

/// Check the English in `text`. Paragraphs in another of the spelling
/// languages are left alone: Harper knows only English.
pub(super) fn check(
    text: &str,
    languages: &[String],
    spelling: bool,
) -> Result<Vec<Issue>, String> {
    let program = program()
        .filter(|p| p.is_file())
        .ok_or_else(|| i18n("Harper is not downloaded. Download it in Settings → Composing."))?;
    let mut codes: Vec<String> = Vec::new();
    for l in languages {
        if let Some(c) = server_code(l, &[]) {
            if !codes.contains(&c) {
                codes.push(c);
            }
        }
    }
    let english = |c: &str| c.to_ascii_lowercase().starts_with("en");
    // A paragraph too short to tell which language it is in is English when
    // every language written in is.
    let all_english = codes.iter().all(|c| english(c));
    let dialect = dialect(codes.iter().find(|c| english(c)).map(String::as_str));
    let mut out = Vec::new();
    for piece in pieces(text, &codes, PIECE) {
        let checked = english(&piece.language) || (piece.language == "auto" && all_english);
        if !checked {
            continue;
        }
        for lint in run(&program, piece.text, dialect)? {
            if spelling && lint.kind == "Spelling" {
                continue;
            }
            let Some(issue) = issue(&lint, piece.text, piece.offset) else {
                continue;
            };
            if super::ignored(&issue.rule, &lint.matched_text) {
                continue;
            }
            out.push(issue);
        }
    }
    Ok(out)
}

/// Harper's name for the kind of English the spelling language is.
fn dialect(code: Option<&str>) -> &'static str {
    let region = code
        .and_then(|c| c.split('-').nth(1))
        .unwrap_or_default()
        .to_ascii_uppercase();
    match region.as_str() {
        "GB" | "IE" | "NZ" | "ZA" => "uk",
        "AU" => "au",
        "CA" => "ca",
        "IN" => "in",
        _ => "us",
    }
}

fn run(program: &std::path::Path, text: &str, dialect: &str) -> Result<Vec<Lint>, String> {
    let dir = dir().unwrap_or_default();
    // Its own dictionary paths, so the user's harper-ls words (if they use
    // it in an editor) neither leak in nor get written to.
    let mut child = Command::new(program)
        .args([
            "lint",
            "--format",
            "json",
            "--quiet",
            "--no-color",
            "-d",
            dialect,
        ])
        .arg("-u")
        .arg(dir.join("dictionary.txt"))
        .arg("-f")
        .arg(dir.join("file-dictionaries"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| i18n_f("Harper could not be started: {e}", &[("e", &e.to_string())]))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A write that fails shows as an answer that does not read.
        let _ = stdin.write_all(text.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|e| i18n_f("Harper could not be started: {e}", &[("e", &e.to_string())]))?;
    // It exits with 1 when it finds something: the answer is what counts.
    let files: Vec<File> = serde_json::from_slice(&output.stdout)
        .map_err(|_| i18n("Harper's answer could not be read."))?;
    Ok(files.into_iter().flat_map(|f| f.lints).collect())
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    lints: Vec<Lint>,
}

#[derive(Deserialize)]
struct Lint {
    #[serde(default)]
    rule: String,
    #[serde(default)]
    kind: String,
    span: Span,
    #[serde(default)]
    message: String,
    #[serde(default)]
    suggestions: Vec<String>,
    #[serde(default)]
    matched_text: String,
}

/// Where a lint is, in characters (Unicode scalar values) of the text.
#[derive(Deserialize)]
struct Span {
    char_start: usize,
    char_end: usize,
}

/// A lint as a problem in the editor's terms: UTF-16 offsets from the start
/// of the whole text, and suggestions as the text to put in its place.
fn issue(lint: &Lint, piece: &str, piece_offset: usize) -> Option<Issue> {
    if lint.span.char_end <= lint.span.char_start {
        return None;
    }
    let mut units = 0usize;
    let (mut start, mut end) = (None, None);
    for (i, c) in piece.chars().enumerate() {
        if i == lint.span.char_start {
            start = Some(units);
        }
        if i == lint.span.char_end {
            end = Some(units);
        }
        units += c.len_utf16();
    }
    let start = start?;
    let end = end.or((lint.span.char_end == piece.chars().count()).then_some(units))?;
    let mut replacements: Vec<String> = Vec::new();
    for s in &lint.suggestions {
        let Some(r) = replacement(s, &lint.matched_text) else {
            continue;
        };
        if !replacements.contains(&r) && r != lint.matched_text {
            replacements.push(r);
        }
    }
    replacements.truncate(SUGGESTIONS);
    Some(Issue {
        offset: piece_offset + start,
        length: end - start,
        message: lint.message.trim().to_string(),
        replacements,
        rule: format!("harper:{}", lint.rule),
        style: matches!(
            lint.kind.as_str(),
            "Style" | "Readability" | "Redundancy" | "Enhancement" | "Regionalism" | "Formatting"
        ),
    })
}

/// The text a suggestion puts in place of `matched`. Harper writes them as
/// "Replace with: “…”", "Insert “…”" (after the problem) and "Remove error".
fn replacement(suggestion: &str, matched: &str) -> Option<String> {
    let quoted = |s: &str| {
        s.strip_prefix('“')
            .and_then(|s| s.strip_suffix('”'))
            .map(str::to_string)
    };
    if let Some(rest) = suggestion.strip_prefix("Replace with: ") {
        return quoted(rest);
    }
    if let Some(rest) = suggestion.strip_prefix("Insert ") {
        return quoted(rest).map(|ins| format!("{matched}{ins}"));
    }
    (suggestion == "Remove error").then(String::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint(start: usize, end: usize, matched: &str, suggestions: &[&str]) -> Lint {
        Lint {
            rule: "PronounVerbAgreement".into(),
            kind: "Agreement".into(),
            span: Span {
                char_start: start,
                char_end: end,
            },
            message: "The verb must agree.".into(),
            suggestions: suggestions.iter().map(|s| s.to_string()).collect(),
            matched_text: matched.into(),
        }
    }

    #[test]
    fn suggestions_become_the_text_to_put_in() {
        assert_eq!(
            replacement("Replace with: “goes”", "go").as_deref(),
            Some("goes")
        );
        assert_eq!(
            replacement("Insert “,”", "Hello").as_deref(),
            Some("Hello,")
        );
        assert_eq!(replacement("Remove error", "the").as_deref(), Some(""));
        assert_eq!(replacement("Something new", "x"), None);
    }

    #[test]
    fn character_spans_become_the_editors_offsets() {
        // "😀" is one character to Harper and two UTF-16 units to the editor.
        let piece = "😀 She go home.";
        let i = issue(
            &lint(
                6,
                8,
                "go",
                &["Replace with: “goes”", "Replace with: “goes”"],
            ),
            piece,
            10,
        )
        .unwrap();
        assert_eq!((i.offset, i.length), (10 + 7, 2));
        assert_eq!(i.replacements, ["goes"]);
        assert_eq!(i.rule, "harper:PronounVerbAgreement");
        assert!(!i.style);
        // A span that runs to the very end of the text.
        let end = issue(&lint(12, 14, "e.", &[]), piece, 0).unwrap();
        assert_eq!(end.offset + end.length, piece.encode_utf16().count());
    }

    #[test]
    fn the_spelling_language_picks_the_dialect() {
        assert_eq!(dialect(Some("en-GB")), "uk");
        assert_eq!(dialect(Some("en-AU")), "au");
        assert_eq!(dialect(Some("en-US")), "us");
        assert_eq!(dialect(Some("en")), "us");
        assert_eq!(dialect(None), "us");
    }

    #[test]
    fn the_answer_is_read() {
        let json = r#"[{"file":"<stdin>","lint_count":1,"lints":[{"rule":"TheirToThere",
            "kind":"Grammar","span":{"char_start":32,"char_end":37},"line":1,"column":33,
            "message":"Did you mean `there`?","priority":31,
            "suggestions":["Replace with: “There”"],"matched_text":"Their"}]}]"#;
        let files: Vec<File> = serde_json::from_str(json).unwrap();
        assert_eq!(files[0].lints[0].span.char_start, 32);
        assert_eq!(files[0].lints[0].matched_text, "Their");
    }

    /// Against a downloaded Harper:
    /// `cargo test -- --ignored live_harper`.
    #[test]
    #[ignore]
    fn live_harper_checks_english_only() {
        let text = "She go home after work every day with her friends.\n\n\
                    Je suis allé a la plage hier avec mes amis et nous avons mangé des glaces.";
        let found = check(text, &["en_US".into(), "fr_FR".into()], true).unwrap();
        let words: Vec<String> = found
            .iter()
            .map(|i| super::super::utf16_slice(text, i.offset, i.length))
            .collect();
        assert_eq!(words, ["go"], "{words:?}");
    }
}
