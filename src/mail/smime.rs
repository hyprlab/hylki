//! S/MIME signing (#478): outgoing mail signed with the user's X.509
//! certificate through `gpgsm`, the S/MIME half of the GnuPG that OpenPGP
//! already goes through. The certificate sits in gpgsm's keybox and its key
//! in gpg-agent, which asks for a passphrase through pinentry; Hylki never
//! sees the key.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::i18n::i18n;

/// A gpgsm to talk to: the user's, or one with its own home (tests).
#[derive(Debug, Clone, Default)]
pub(crate) struct Gpgsm {
    /// `GNUPGHOME` for the child process; `None` = the user's own.
    home: Option<PathBuf>,
}

impl Gpgsm {
    /// The user's gpgsm, honouring `HYLKI_GNUPGHOME` as OpenPGP does.
    pub(crate) fn system() -> Gpgsm {
        Gpgsm {
            home: std::env::var_os("HYLKI_GNUPGHOME").map(PathBuf::from),
        }
    }

    #[cfg(test)]
    pub(crate) fn at(home: PathBuf) -> Gpgsm {
        Gpgsm { home: Some(home) }
    }

    pub(crate) fn command(&self) -> Command {
        let mut c = Command::new("gpgsm");
        c.args(["--batch", "--no-tty", "--status-fd", "2"]);
        if let Some(h) = &self.home {
            c.env("GNUPGHOME", h);
        }
        c
    }
}

/// A detached CMS signature (DER) over `data` by the certificate gpgsm
/// finds for `signer` (an address or fingerprint), with the `micalg` value
/// RFC 8551 wants beside it ("sha-256"), read from gpgsm's SIG_CREATED line.
/// The certificate chain, all but the root, travels inside the signature.
pub(crate) fn sign_detached(
    gpgsm: &Gpgsm,
    data: &[u8],
    signer: &str,
) -> Result<(Vec<u8>, &'static str), String> {
    let mut child = gpgsm
        .command()
        .args(["--detach-sign", "--local-user", signer])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("gpgsm: {e}"))?;
    // Fed from a thread: gpgsm may write before it has read everything, and
    // a pipe full both ways is a deadlock.
    let stdin = child.stdin.take();
    let input = data.to_vec();
    let feeder = std::thread::spawn(move || {
        if let Some(mut s) = stdin {
            let _ = s.write_all(&input);
        }
    });
    let out = child
        .wait_with_output()
        .map_err(|e| format!("gpgsm: {e}"))?;
    let _ = feeder.join();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let created = stderr
        .lines()
        .find_map(|l| l.strip_prefix("[GNUPG:] SIG_CREATED "));
    match created {
        Some(rest) if out.status.success() && !out.stdout.is_empty() => {
            Ok((out.stdout, micalg(rest)))
        }
        _ => Err(stderr
            .lines()
            .rfind(|l| !l.starts_with("[GNUPG:]") && !l.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| i18n("gpgsm did not sign the message."))),
    }
}

/// RFC 8551's name for the hash in `SIG_CREATED <type> <pk algo> <hash
/// algo> …`, which gives libgcrypt's number for it.
fn micalg(sig_created: &str) -> &'static str {
    match sig_created.split(' ').nth(2) {
        Some("2") => "sha-1",
        Some("9") => "sha-384",
        Some("10") => "sha-512",
        Some("11") => "sha-224",
        _ => "sha-256",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micalg_follows_the_hash_gpgsm_used() {
        assert_eq!(micalg("D 1 8 00 20261123T144859 FPR"), "sha-256");
        assert_eq!(micalg("D 1 10 00 20261123T144859 FPR"), "sha-512");
        assert_eq!(micalg("D 1 2 00 20261123T144859 FPR"), "sha-1");
    }
}
