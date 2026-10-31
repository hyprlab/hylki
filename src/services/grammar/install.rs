//! Downloading a checker the user asked for (#452): Harper, or LanguageTool
//! and the Java runtime it needs. Nothing of either ships with the app.
//!
//! Every archive is pinned by address, size and SHA-256, so what runs is
//! exactly what was tested, and a changed or damaged download is refused
//! before it is unpacked. An install is put together beside its folder and
//! moved into place whole, so a cancelled or failed one leaves nothing
//! half-written, and the version file written last is what says it is there.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::i18n::{i18n, i18n_f};

/// One file to fetch.
pub(crate) struct Archive {
    pub(crate) url: &'static str,
    pub(crate) size: u64,
    pub(crate) sha256: &'static str,
    pub(crate) kind: Kind,
    /// The folder of the install it is unpacked to.
    pub(crate) folder: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    TarGz,
    Zip,
}

/// How far a download has got, read by Settings while it runs, and how it
/// is told to stop.
#[derive(Default)]
pub(crate) struct Progress {
    pub(crate) done: AtomicU64,
    pub(crate) total: AtomicU64,
    pub(crate) cancel: AtomicBool,
}

/// Where the checkers are kept: `~/.local/share/hylki/grammar/<name>`.
pub(crate) fn folder(name: &str) -> Option<PathBuf> {
    Some(
        crate::config::data_base()?
            .join("hylki")
            .join("grammar")
            .join(name),
    )
}

const VERSION_FILE: &str = "version";

/// The version installed in `dir`, if a whole install is there.
pub(crate) fn installed(dir: &Path) -> Option<String> {
    let v = std::fs::read_to_string(dir.join(VERSION_FILE)).ok()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Fetch, check and unpack `archives` into `dir` as `version` (blocking).
/// `space` is what the whole install needs on disk, archives included.
pub(crate) fn install(
    dir: &Path,
    version: &str,
    archives: &[&Archive],
    space: u64,
    progress: &Progress,
) -> Result<(), String> {
    let parent = dir
        .parent()
        .ok_or_else(|| i18n("There is no folder to install to."))?;
    std::fs::create_dir_all(parent).map_err(|e| write_failed(&e))?;
    if let Some(free) = free_space(parent) {
        if free < space {
            return Err(i18n_f(
                "There is not enough free space: {size} is needed.",
                &[("size", &megabytes(space))],
            ));
        }
    }
    progress
        .total
        .store(archives.iter().map(|a| a.size).sum(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let staging = dir.with_extension("partial");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| write_failed(&e))?;
    let result = (|| {
        for archive in archives {
            let file = staging.join(format!("{}.download", archive.folder));
            download(archive, &file, progress)?;
            unpack(archive, &file, &staging.join(archive.folder))?;
            let _ = std::fs::remove_file(&file);
        }
        std::fs::write(staging.join(VERSION_FILE), version).map_err(|e| write_failed(&e))?;
        let _ = std::fs::remove_dir_all(dir);
        std::fs::rename(&staging, dir).map_err(|e| write_failed(&e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Take an install away.
pub(crate) fn remove(dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(write_failed(&e)),
    }
}

fn download(archive: &Archive, to: &Path, progress: &Progress) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(60))
        .build();
    let resp = agent.get(archive.url).call().map_err(|e| {
        i18n_f(
            "The download could not start: {e}",
            &[("e", &e.to_string())],
        )
    })?;
    let mut reader = resp.into_reader();
    let mut out = std::fs::File::create(to).map_err(|e| write_failed(&e))?;
    let mut hash = Sha256::new();
    let mut got = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(i18n("The download was cancelled."));
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| i18n_f("The download stopped: {e}", &[("e", &e.to_string())]))?;
        if n == 0 {
            break;
        }
        // Past the size it should be, it is not the file that was tested.
        got += n as u64;
        if got > archive.size {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| write_failed(&e))?;
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    out.flush().map_err(|e| write_failed(&e))?;
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != archive.size || digest != archive.sha256 {
        tracing::warn!(
            "grammar download {} did not match: {got} bytes, sha256 {digest}",
            archive.url
        );
        return Err(i18n(
            "The downloaded file is not the one expected, so it was not installed.",
        ));
    }
    Ok(())
}

/// Unpack `file` into `to`. An archive that holds a single folder (as
/// LanguageTool's and Java's do) has that folder become `to`.
fn unpack(archive: &Archive, file: &Path, to: &Path) -> Result<(), String> {
    let tmp = to.with_extension("unpack");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| write_failed(&e))?;
    let unpacked = match archive.kind {
        Kind::TarGz => std::fs::File::open(file)
            .map(flate2::read::GzDecoder::new)
            .map(tar::Archive::new)
            .and_then(|mut a| {
                a.set_preserve_permissions(true);
                a.unpack(&tmp)
            })
            .map_err(|e| e.to_string()),
        Kind::Zip => std::fs::File::open(file)
            .map_err(|e| e.to_string())
            .and_then(|f| zip::ZipArchive::new(f).map_err(|e| e.to_string()))
            .and_then(|mut z| z.extract(&tmp).map_err(|e| e.to_string())),
    };
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(i18n_f(
            "The download could not be unpacked: {e}",
            &[("e", &e)],
        ));
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(&tmp)
        .map_err(|e| write_failed(&e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    let inner = match entries.as_slice() {
        [one] if one.is_dir() => one.clone(),
        _ => tmp.clone(),
    };
    std::fs::rename(&inner, to).map_err(|e| write_failed(&e))?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a NUL-terminated string and `st` a struct statvfs
    // owned here, which the call fills in.
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

fn write_failed(e: &std::io::Error) -> String {
    i18n_f(
        "The files could not be written: {e}",
        &[("e", &e.to_string())],
    )
}

/// A size as people read it: "13 MB", "304 MB".
pub(crate) fn megabytes(bytes: u64) -> String {
    i18n_f("{n} MB", &[("n", &(bytes.div_ceil(1_000_000)).to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_gz(dir: &Path) -> (PathBuf, u64, String) {
        let path = dir.join("a.tar.gz");
        let f = std::fs::File::create(&path).unwrap();
        let mut t = tar::Builder::new(flate2::write::GzEncoder::new(
            f,
            flate2::Compression::fast(),
        ));
        let body = b"#!/bin/sh\necho hi\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        t.append_data(&mut h, "tool-1.0/bin/tool", &body[..])
            .unwrap();
        t.into_inner().unwrap().finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let sum: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (path, bytes.len() as u64, sum)
    }

    #[test]
    fn a_single_folder_becomes_the_install_folder_and_keeps_its_modes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hylki-gr-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (file, _, _) = tar_gz(&dir);
        let archive = Archive {
            url: "",
            size: 0,
            sha256: "",
            kind: Kind::TarGz,
            folder: "tool",
        };
        unpack(&archive, &file, &dir.join("tool")).unwrap();
        let bin = dir.join("tool/bin/tool");
        assert!(bin.is_file());
        assert_eq!(
            std::fs::metadata(&bin).unwrap().permissions().mode() & 0o111,
            0o111
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_zip_unpacks_into_the_install_folder() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join(format!("hylki-gr-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        z.start_file("LanguageTool-6.6/languagetool-server.jar", opts)
            .unwrap();
        z.write_all(b"jar").unwrap();
        z.finish().unwrap();
        let archive = Archive {
            url: "",
            size: 0,
            sha256: "",
            kind: Kind::Zip,
            folder: "languagetool",
        };
        unpack(&archive, &file, &dir.join("languagetool")).unwrap();
        assert_eq!(
            std::fs::read(dir.join("languagetool/languagetool-server.jar")).unwrap(),
            b"jar"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_read_in_megabytes() {
        assert_eq!(megabytes(13_445_972), "14 MB");
        assert_eq!(megabytes(1), "1 MB");
    }

    #[test]
    fn a_folder_without_a_version_file_is_not_installed() {
        let dir = std::env::temp_dir().join(format!("hylki-gr-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(installed(&dir), None);
        std::fs::write(dir.join(VERSION_FILE), "3.0.0\n").unwrap();
        assert_eq!(installed(&dir).as_deref(), Some("3.0.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
