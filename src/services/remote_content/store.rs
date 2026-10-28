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

fn dir() -> Option<PathBuf> {
    crate::config::cache_base().map(|b| b.join("remote"))
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
    std::fs::create_dir_all(dir)?;
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
    std::fs::write(&tmp, &data)?;
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
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        // Stale entries, and a `.part` left by a write that never finished.
        let stale = now.duration_since(modified).unwrap_or_default() >= MAX_AGE;
        let partial = e.path().extension().is_some_and(|x| x == "part")
            && now.duration_since(modified).unwrap_or_default() > Duration::from_secs(600);
        if stale || partial {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
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
