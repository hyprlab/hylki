//! OpenPGP (#133), first slice: decrypt and verify what arrives, through the
//! user's own GnuPG — `gpg` on the path, the keyring in `~/.gnupg`, the agent
//! (and its pinentry) for passphrases. Hylki never sees a secret key and never
//! writes anything decrypted to disk: the worker renders a decrypted message
//! straight to the reader and leaves the cache alone.
//!
//! What is recognised: PGP/MIME (RFC 3156) `multipart/encrypted` and
//! `multipart/signed`, and the older inline forms — an armoured
//! `-----BEGIN PGP MESSAGE-----` or a clear-signed block in the text body.
//! Signing and encrypting outgoing mail, and key management, came in later
//! slices.
//!
//! This file runs gpg; recognising and unwrapping incoming mail is in
//! `inbound`, reading gpg's status lines in `status`, the keyring in `keys`,
//! signing and encrypting in `outbound`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub(crate) mod autocrypt;
pub(crate) mod discovery;
mod inbound;
mod keys;
mod outbound;
mod status;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use inbound::Shape;
pub(crate) use inbound::{detect, is_encrypted, legacy_display_part, unwrap_message};
pub(crate) use keys::{
    delete_key, export_public, fetch_key, generate_key, import_keys, key_by_fingerprint,
    key_display, list_keys, public_key_for, secret_key_by_fingerprint, secret_key_for,
    still_unchecked, trust_key, ImportSummary, KeyInfo,
};
pub(crate) use outbound::{encrypt, sign_detached, Recipient};

/// A GnuPG to talk to: the system one by default, or one with its own home
/// directory (tests build a throwaway keyring).
#[derive(Debug, Clone, Default)]
pub(crate) struct Gpg {
    /// `GNUPGHOME` for the child process; `None` = the user's own.
    home: Option<PathBuf>,
}

impl Gpg {
    /// The user's GnuPG (honouring `HYLKI_GNUPGHOME`, for trying an
    /// alternative keyring without touching the real one).
    pub(crate) fn system() -> Gpg {
        Gpg {
            home: std::env::var_os("HYLKI_GNUPGHOME").map(PathBuf::from),
        }
    }

    /// A throwaway keyring in `home`, made 0700 when it is new, for tests.
    #[cfg(test)]
    pub(crate) fn at(home: PathBuf) -> Gpg {
        Gpg::private_home(home).expect("a test keyring")
    }

    /// Hylki's own keyring for keys it found itself: from Autocrypt
    /// headers, gossip and WKD. They are kept out of the user's keyring
    /// because nobody chose them, and a key there is one gpg would pick
    /// for that address everywhere else too. `None` without a data
    /// directory.
    pub(crate) fn discovered() -> Option<Gpg> {
        Gpg::system().found_keyring()
    }

    /// The keyring of found keys that goes with this one: beside an
    /// alternative home (`HYLKI_GNUPGHOME`, a test's), so a trial keyring
    /// keeps its discoveries to itself, else in Hylki's data directory.
    fn found_home(&self) -> Option<PathBuf> {
        match &self.home {
            Some(h) => Some(h.join("hylki-discovered")),
            None => Some(crate::config::data_base()?.join("hylki/discovered-keys")),
        }
    }

    /// [`Gpg::found_home`]'s keyring, made if it is not there yet.
    pub(crate) fn found_keyring(&self) -> Option<Gpg> {
        Gpg::private_home(self.found_home()?)
    }

    /// [`Gpg::found_home`]'s keyring, if anything was ever found.
    pub(crate) fn found_keyring_if_any(&self) -> Option<Gpg> {
        let home = self.found_home().filter(|h| h.is_dir())?;
        Some(Gpg { home: Some(home) })
    }

    /// A keyring in `home`, made 0700 first when it is new. gpg refuses
    /// a home others can read, and making the directory here rather than
    /// letting gpg do it keeps a new gpg from setting it up for keyboxd,
    /// a daemon per keyring that would outlive every lookup.
    fn private_home(home: PathBuf) -> Option<Gpg> {
        if !home.is_dir() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            if let Err(e) = builder.create(&home) {
                tracing::warn!(target: "hylki::pgp", "cannot make {}: {e}", home.display());
                return None;
            }
        }
        Some(Gpg { home: Some(home) })
    }

    fn command(&self) -> Command {
        let mut c = Command::new("gpg");
        // `--batch` keeps gpg off the terminal; the agent still asks for a
        // passphrase through pinentry, which is exactly the prompt we want.
        c.args([
            "--batch",
            "--no-tty",
            "--status-fd",
            "2",
            "--exit-on-status-write-error",
        ]);
        if let Some(h) = &self.home {
            c.env("GNUPGHOME", h);
        }
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
}

/// Whether a `gpg` answers at all. Checked once per process.
pub(crate) fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("gpg")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// What one gpg run produced.
#[derive(Debug, Default)]
struct Run {
    stdout: Vec<u8>,
    /// The `[GNUPG:] …` machine lines, prefix stripped.
    status_lines: Vec<String>,
    /// The last human line gpg printed, for a failure nobody parsed.
    detail: Option<String>,
}

/// Run gpg with `input` on stdin and, for a detached verification, the
/// signature in a temporary file (gpg takes the signature as a path and the
/// data on stdin; the other way round is not offered).
fn run(gpg: &Gpg, args: &[&str], input: &[u8], detached_sig: Option<&[u8]>) -> Run {
    let mut cmd = gpg.command();
    cmd.args(args);
    let sig_path = detached_sig.map(|sig| {
        let path = std::env::temp_dir().join(format!(
            "vireo-sig-{}-{}.asc",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::write(&path, sig);
        path
    });
    if let Some(p) = &sig_path {
        cmd.arg(p);
        cmd.arg("-");
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Some(p) = &sig_path {
                let _ = std::fs::remove_file(p);
            }
            return Run {
                detail: Some(format!("gpg: {e}")),
                ..Default::default()
            };
        }
    };
    // Feed stdin from a thread: gpg may start writing before it has read
    // everything, and a pipe full both ways is a deadlock.
    let stdin = child.stdin.take();
    let input = input.to_vec();
    let feeder = std::thread::spawn(move || {
        if let Some(mut s) = stdin {
            let _ = s.write_all(&input);
        }
    });
    let output = child.wait_with_output();
    let _ = feeder.join();
    if let Some(p) = &sig_path {
        let _ = std::fs::remove_file(p);
    }
    let Ok(output) = output else {
        return Run {
            detail: Some("gpg: no output".into()),
            ..Default::default()
        };
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A lock left by a gpg or keyboxd that is no longer serving it makes
    // every call wait about ten seconds and then fail (#316); say so at a
    // level the exported log keeps.
    if let Some(line) = stderr.lines().find(|l| l.contains("waiting for lock")) {
        tracing::warn!(
            target: "hylki::pgp",
            "gpg {} waited on a keyring lock: {}",
            args.last().copied().unwrap_or(""),
            line.trim_start_matches("gpg: ").trim()
        );
    }
    let mut status_lines = Vec::new();
    let mut detail = None;
    for line in stderr.lines() {
        if let Some(s) = line.strip_prefix("[GNUPG:] ") {
            status_lines.push(s.to_string());
        } else if !line.trim().is_empty() {
            let l = line.trim_start_matches("gpg: ").trim().to_string();
            tracing::debug!(target: "hylki::pgp", "gpg: {l}");
            detail = Some(l);
        }
    }
    for s in &status_lines {
        tracing::debug!(target: "hylki::pgp", "[GNUPG:] {s}");
    }
    Run {
        stdout: output.stdout,
        status_lines,
        detail,
    }
}

/// The key ids an encrypted message names (all zeros for a hidden
/// recipient), without decrypting it. For tests.
#[cfg(test)]
pub(crate) fn encrypted_to(gpg: &Gpg, armored: &[u8]) -> Vec<String> {
    run(gpg, &["--list-only", "--decrypt"], armored, None)
        .status_lines
        .iter()
        .filter_map(|l| l.strip_prefix("ENC_TO "))
        .map(|r| r.split(' ').next().unwrap_or("").to_string())
        .collect()
}

/// Stop the daemons a test keyring started, and remove it. For tests.
#[cfg(test)]
pub(crate) fn drop_test_keyring(home: &std::path::Path) {
    let _ = Command::new("gpgconf")
        .env("GNUPGHOME", home)
        .args(["--kill", "all"])
        .status();
    let _ = std::fs::remove_dir_all(home);
}
