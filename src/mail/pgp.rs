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

mod inbound;
mod keys;
mod outbound;
mod status;
#[cfg(test)]
mod tests;

pub(crate) use inbound::{detect, is_encrypted, legacy_display_part, unwrap_message};
pub(crate) use keys::{
    delete_key, export_public, fetch_key, generate_key, import_keys, key_by_fingerprint,
    key_display, list_keys, public_key_for, secret_key_by_fingerprint, secret_key_for, trust_key,
    ImportSummary, KeyInfo,
};
pub(crate) use outbound::{encrypt, sign_detached};

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
