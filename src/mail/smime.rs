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
    /// The user's gpgsm, honoring `HYLKI_GNUPGHOME` as OpenPGP does.
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

/// Why gpgsm would not sign, from the reason on its `INV_SGNR` line, so
/// the sender hears what to do rather than gpgsm's last words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignError {
    /// No certificate with a private key names the address.
    NoCertificate,
    Revoked,
    Expired,
    /// Its chain ends at a root missing from `trustlist.txt`.
    NotTrusted,
    /// Anything else, in gpgsm's words.
    Other(String),
}

/// A detached CMS signature (DER) over `data` by the certificate gpgsm
/// finds for `signer` (an address or fingerprint), with the `micalg` value
/// RFC 8551 wants beside it ("sha-256"), read from gpgsm's SIG_CREATED line.
/// The certificate chain, all but the root, travels inside the signature.
///
/// gpgsm checks the certificate's revocation list before it signs, and
/// refuses when it cannot fetch one: offline, behind a proxy, or with a
/// certificate authority that is down. That check is tried first; when it
/// is what failed, the signature is made without it, as Thunderbird goes
/// ahead when a revocation server does not answer. A certificate the list
/// says is revoked is still refused.
pub(crate) fn sign_detached(
    gpgsm: &Gpgsm,
    data: &[u8],
    signer: &str,
) -> Result<(Vec<u8>, &'static str), SignError> {
    match run_sign(gpgsm, data, signer, false) {
        Err(Refused {
            crl_failed: true, ..
        }) => {
            tracing::info!("gpgsm could not check the revocation list; signing without it");
            run_sign(gpgsm, data, signer, true).map_err(|r| r.error)
        }
        other => other.map_err(|r| r.error),
    }
}

struct Refused {
    error: SignError,
    /// The revocation list could not be fetched or was out of date.
    crl_failed: bool,
}

fn run_sign(
    gpgsm: &Gpgsm,
    data: &[u8],
    signer: &str,
    skip_crl: bool,
) -> Result<(Vec<u8>, &'static str), Refused> {
    let mut command = gpgsm.command();
    if skip_crl {
        command.arg("--disable-crl-checks");
    }
    let spawned = command
        .args(["--detach-sign", "--local-user", signer])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let other = |e: std::io::Error| Refused {
        error: SignError::Other(format!("gpgsm: {e}")),
        crl_failed: false,
    };
    let mut child = spawned.map_err(other)?;
    // Fed from a thread: gpgsm may write before it has read everything, and
    // a pipe full both ways is a deadlock.
    let stdin = child.stdin.take();
    let input = data.to_vec();
    let feeder = std::thread::spawn(move || {
        if let Some(mut s) = stdin {
            let _ = s.write_all(&input);
        }
    });
    let out = child.wait_with_output().map_err(other)?;
    let _ = feeder.join();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let created = stderr
        .lines()
        .find_map(|l| l.strip_prefix("[GNUPG:] SIG_CREATED "));
    match created {
        Some(rest) if out.status.success() && !out.stdout.is_empty() => {
            Ok((out.stdout, micalg(rest)))
        }
        _ => Err(refusal(&stderr)),
    }
}

/// What gpgsm's status and log lines say went wrong. The `INV_SGNR`
/// reasons are GnuPG's (doc/DETAILS); 0 is "no specific reason", which is
/// what a failed revocation-list fetch gives, so that one is told from
/// the log line gpgsm writes for it.
fn refusal(stderr: &str) -> Refused {
    let reason = stderr
        .lines()
        .find_map(|l| l.strip_prefix("[GNUPG:] INV_SGNR "))
        .and_then(|rest| rest.split(' ').next());
    // Only a refusal with no other reason: a revoked certificate stays
    // refused whatever else the log says.
    let crl_failed = match reason {
        Some("6" | "7") => true,
        Some("0") | None => {
            stderr.contains("checking the CRL failed") || stderr.contains("dirmngr")
        }
        _ => false,
    };
    let error = match reason {
        Some("1" | "9" | "11") => SignError::NoCertificate,
        Some("4") => SignError::Revoked,
        Some("5") => SignError::Expired,
        Some("10" | "12") => SignError::NotTrusted,
        _ => SignError::Other(
            stderr
                .lines()
                .rfind(|l| !l.starts_with("[GNUPG:]") && !l.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| i18n("gpgsm did not sign the message.")),
        ),
    };
    Refused { error, crl_failed }
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

    /// gpgsm 2.4.9's own output, from a certificate missing from the
    /// keybox and from one whose revocation list could not be fetched.
    #[test]
    fn refusals_are_told_apart() {
        let missing = "gpgsm: can't sign using 'a@b.invalid': No public key\n\
                       [GNUPG:] INV_SGNR 1 a@b.invalid\n[GNUPG:] INV_RECP 1 a@b.invalid\n";
        let r = refusal(missing);
        assert_eq!(r.error, SignError::NoCertificate);
        assert!(!r.crl_failed);

        let crl = "gpgsm: certificate #2F27/CN=Test CA\n\
                   gpgsm: checking the CRL failed: Server indicated a failure\n\
                   gpgsm: can't sign using 'a@b.invalid': Server indicated a failure\n\
                   [GNUPG:] INV_SGNR 0 a@b.invalid\n";
        let r = refusal(crl);
        assert!(r.crl_failed);
        assert_eq!(
            r.error,
            SignError::Other(
                "gpgsm: can't sign using 'a@b.invalid': Server indicated a failure".into()
            )
        );

        let revoked = "gpgsm: dirmngr cache-only key lookup\n[GNUPG:] INV_SGNR 4 a@b.invalid\n";
        let r = refusal(revoked);
        assert_eq!(r.error, SignError::Revoked);
        assert!(!r.crl_failed, "a revoked certificate is not retried");
    }
}
