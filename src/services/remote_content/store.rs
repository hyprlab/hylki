//! The disk side of the remote-content cache: one file per URL under
//! `~/.cache/hylki/remote`, named by the URL's SHA-256, holding the content
//! type on its first line and the bytes after it.
//!
//! An entry is used for a week from its fetch, then fetched again; pictures
//! in mail rarely change, but some do (a sale's countdown, a weather card).
//! Past `LIMIT` the oldest fetches go first, down to nine tenths of it, so a
//! trim is not run again on the very next write.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// The most the cache keeps on disk.
pub(super) const LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// How long an entry is used before it is fetched again.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Bytes on disk as far as this session knows: set by the first trim, then
/// added to by every write.
static TOTAL: AtomicU64 = AtomicU64::new(0);
static TRIMMED_ONCE: AtomicBool = AtomicBool::new(false);
static TRIMMING: AtomicBool = AtomicBool::new(false);

/// What a URL answered with.
#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) content_type: String,
    pub(crate) bytes: Vec<u8>,
}

/// The cache folder, `~/.cache/hylki/remote`. Blocking: the first call of a
/// session clears out the folder earlier releases used.
fn dir() -> Option<PathBuf> {
    static RETIRED: std::sync::Once = std::sync::Once::new();
    let base = crate::config::cache_base()?;
    RETIRED.call_once(|| retire(&base.join("remote")));
    Some(base.join("hylki").join("remote"))
}

/// Releases from 1.46.0 kept the cache in `~/.cache/remote` (#614), a name
/// other programs may use too: only Hylki's own entries are taken out of it,
/// and the folder goes only if that leaves it empty.
fn retire(old: &Path) {
    let Ok(read) = std::fs::read_dir(old) else {
        return;
    };
    for e in read.flatten() {
        if is_entry(&e.file_name()) && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let _ = std::fs::remove_dir(old);
}

/// Whether a file is one the cache wrote: a SHA-256 in lower-case hex, or
/// one still being written. Anything else in the folder is left alone.
fn is_entry(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let hash = name.strip_suffix(".part").unwrap_or(name);
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn file_name(url: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(url.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The fresh entry for `url`, if there is one. Blocking.
pub(super) fn read(url: &str) -> Option<Entry> {
    read_in(&dir()?, url, SystemTime::now())
}

/// Whether a fresh entry for `url` is on disk. Blocking.
pub(super) fn has(url: &str) -> bool {
    dir().is_some_and(|d| fresh(&d.join(file_name(url)), SystemTime::now()))
}

fn fresh(path: &Path, now: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| now.duration_since(t).unwrap_or_default() < MAX_AGE)
}

fn read_in(dir: &Path, url: &str, now: SystemTime) -> Option<Entry> {
    let path = dir.join(file_name(url));
    if !fresh(&path, now) {
        return None;
    }
    let raw = std::fs::read(path).ok()?;
    let nl = raw.iter().position(|&b| b == b'\n')?;
    Some(Entry {
        content_type: String::from_utf8_lossy(&raw[..nl]).into_owned(),
        bytes: raw[nl + 1..].to_vec(),
    })
}

/// Keep `entry` for `url`, trimming the cache when it has grown past
/// `LIMIT`. Blocking.
pub(super) fn write(url: &str, entry: &Entry) {
    let Some(dir) = dir() else { return };
    if !TRIMMED_ONCE.swap(true, Ordering::Relaxed) {
        trim_in(&dir, LIMIT, SystemTime::now());
    }
    match write_in(&dir, url, entry) {
        Ok(n) => {
            if TOTAL.fetch_add(n, Ordering::Relaxed) + n > LIMIT {
                trim_in(&dir, LIMIT, SystemTime::now());
            }
        }
        Err(e) => tracing::debug!("remote content not cached: {e}"),
    }
}

fn write_in(dir: &Path, url: &str, entry: &Entry) -> std::io::Result<u64> {
    // What was fetched for a message says which mail was opened: the
    // directory and its files are the owner's alone.
    crate::config::private_dir(dir)?;
    let name = file_name(url);
    // Written aside and renamed into place, so a reader never sees half a file.
    let tmp = dir.join(format!("{name}.part"));
    let content_type: String = entry
        .content_type
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let mut data = Vec::with_capacity(content_type.len() + 1 + entry.bytes.len());
    data.extend_from_slice(content_type.as_bytes());
    data.push(b'\n');
    data.extend_from_slice(&entry.bytes);
    crate::config::write_private_bytes(&tmp, &data)?;
    std::fs::rename(&tmp, dir.join(name))?;
    Ok(data.len() as u64)
}

/// Remove stale entries, then the oldest until the cache is under nine
/// tenths of `limit`, and record what is left. One trim at a time.
fn trim_in(dir: &Path, limit: u64, now: SystemTime) {
    if TRIMMING.swap(true, Ordering::Relaxed) {
        return;
    }
    let left = trim_files(dir, limit, now);
    TOTAL.store(left, Ordering::Relaxed);
    TRIMMING.store(false, Ordering::Relaxed);
}

fn trim_files(dir: &Path, limit: u64, now: SystemTime) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for e in read.flatten() {
        let name = e.file_name();
        if !is_entry(&name) {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        // Stale entries, and a `.part` left by a write that never finished.
        let stale = now.duration_since(modified).unwrap_or_default() >= MAX_AGE;
        let partial = name.to_str().is_some_and(|n| n.ends_with(".part"))
            && now.duration_since(modified).unwrap_or_default() > Duration::from_secs(600);
        if stale || partial {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        tighten(&e.path(), &meta);
        files.push((modified, meta.len(), e.path()));
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total > limit {
        files.sort_unstable_by_key(|f| f.0);
        let target = limit / 10 * 9;
        for (_, len, path) in files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
    total
}

/// An entry written before the cache was kept private is made so at the
/// first trim of a session.
#[cfg(unix)]
fn tighten(path: &Path, meta: &std::fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o077 != 0 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

#[cfg(not(unix))]
fn tighten(_path: &Path, _meta: &std::fs::Metadata) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hylki-remote-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn an_entry_reads_back_until_it_is_a_week_old() {
        let dir = temp_dir("roundtrip");
        let entry = Entry {
            content_type: "image/png".into(),
            bytes: b"\x89PNG\n\x00rest".to_vec(),
        };
        write_in(&dir, "https://a.example/x.png", &entry).unwrap();
        let now = SystemTime::now();
        let back = read_in(&dir, "https://a.example/x.png", now).unwrap();
        assert_eq!(back.content_type, "image/png");
        assert_eq!(back.bytes, entry.bytes);
        assert!(read_in(&dir, "https://a.example/y.png", now).is_none());
        assert!(read_in(&dir, "https://a.example/x.png", now + MAX_AGE).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_cache_is_its_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = temp_dir("private");
        // As an earlier version left it: open to every user.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old = dir.join(file_name("https://a.example/old.png"));
        std::fs::write(&old, b"image/png\nold").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
        let entry = Entry {
            content_type: "image/png".into(),
            bytes: b"new".to_vec(),
        };
        write_in(&dir, "https://a.example/new.png", &entry).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(
            mode(&dir.join(file_name("https://a.example/new.png"))),
            0o600
        );
        trim_files(&dir, LIMIT, SystemTime::now());
        assert_eq!(mode(&old), 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_caches_own_files_are_touched() {
        let dir = temp_dir("foreign");
        std::fs::create_dir_all(&dir).unwrap();
        let ours = dir.join(file_name("https://a.example/x.png"));
        let part = dir.join(format!("{}.part", file_name("https://a.example/y.png")));
        let theirs = dir.join("settings.json");
        let upper = dir.join(file_name("https://a.example/z.png").to_uppercase());
        for p in [&ours, &part, &theirs, &upper] {
            std::fs::write(p, b"x").unwrap();
        }
        // A week on, a trim takes what the cache wrote and nothing else.
        trim_files(&dir, LIMIT, SystemTime::now() + MAX_AGE);
        assert!(!ours.exists() && !part.exists());
        assert!(theirs.exists() && upper.exists());
        // Retiring a folder another program also uses keeps that program's
        // files, and the folder with them.
        std::fs::write(&ours, b"x").unwrap();
        retire(&dir);
        assert!(!ours.exists());
        assert!(theirs.exists() && upper.exists());
        std::fs::remove_file(&theirs).unwrap();
        std::fs::remove_file(&upper).unwrap();
        // With only Hylki's entries in it, the folder goes too.
        std::fs::write(&ours, b"x").unwrap();
        retire(&dir);
        assert!(!dir.exists());
    }

    #[test]
    fn a_trim_drops_the_oldest_and_the_stale() {
        let dir = temp_dir("trim");
        let entry = Entry {
            content_type: String::new(),
            bytes: vec![0; 999],
        };
        for n in 0..10 {
            write_in(&dir, &format!("https://a.example/{n}"), &entry).unwrap();
            let path = dir.join(file_name(&format!("https://a.example/{n}")));
            let at = SystemTime::now() - Duration::from_secs(1000 - n * 10);
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        // Ten files of 1000 bytes against a limit of 5000: down to 4500.
        let left = trim_files(&dir, 5000, SystemTime::now());
        assert_eq!(left, 4000);
        let now = SystemTime::now();
        assert!(read_in(&dir, "https://a.example/0", now).is_none());
        assert!(read_in(&dir, "https://a.example/9", now).is_some());
        // A week on, nothing is left.
        assert_eq!(trim_files(&dir, 5000, now + MAX_AGE), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
