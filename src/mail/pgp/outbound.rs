//! Signing and encrypting outgoing mail.

use super::{run, Gpg};
use crate::i18n::i18n;

/// A detached, armoured signature over `data` by `local_user`, with the
/// `micalg` value RFC 3156 wants beside it ("pgp-sha256"), read from gpg's
/// SIG_CREATED line.
pub(crate) fn sign_detached(
    gpg: &Gpg,
    data: &[u8],
    local_user: &str,
) -> Result<(Vec<u8>, String), String> {
    let out = run(
        gpg,
        &["--armor", "--local-user", local_user, "--detach-sign"],
        data,
        None,
    );
    if out.stdout.is_empty() {
        return Err(out
            .detail
            .unwrap_or_else(|| i18n("gpg did not sign the message.")));
    }
    // SIG_CREATED <type> <pk algo> <hash algo> <class> <timestamp> <fpr>
    let hash = out
        .status_lines
        .iter()
        .find_map(|l| l.strip_prefix("SIG_CREATED "))
        .and_then(|rest| rest.split(' ').nth(2))
        .and_then(|h| h.parse::<u32>().ok())
        .unwrap_or(8);
    let micalg = match hash {
        1 => "pgp-md5",
        2 => "pgp-sha1",
        3 => "pgp-ripemd160",
        9 => "pgp-sha384",
        10 => "pgp-sha512",
        11 => "pgp-sha224",
        _ => "pgp-sha256",
    };
    Ok((out.stdout, micalg.to_string()))
}

/// Who a message is encrypted to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Recipient {
    /// An address or fingerprint gpg finds in the user's keyring.
    Id(String),
    /// A key handed over as bytes: one from Hylki's own keyring of
    /// discovered keys (Autocrypt, WKD), which the user's gpg never holds.
    Key(Vec<u8>),
}

/// `data` encrypted (and, with `local_user`, signed) to every recipient,
/// armoured. The sender is a recipient too, so the Sent copy stays
/// readable. Keys are used as found: a recipient's key that is in the
/// keyring but not vouched for still encrypts, as Thunderbird does.
///
/// `hidden` recipients (Bcc) go in with their key ids zeroed: every copy of
/// the message carries the same encrypted packet, so a Bcc key id written
/// out in it would tell the To and Cc who else could read it.
pub(crate) fn encrypt(
    gpg: &Gpg,
    data: &[u8],
    recipients: &[Recipient],
    hidden: &[Recipient],
    local_user: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut files = KeyFiles::default();
    let mut args: Vec<String> = vec![
        "--armor".into(),
        "--trust-model".into(),
        "always".into(),
        "--encrypt".into(),
    ];
    if let Some(u) = local_user {
        args.extend(["--sign".into(), "--local-user".into(), u.into()]);
    }
    for (list, is_hidden) in [(recipients, false), (hidden, true)] {
        for r in list {
            let (flag, value) = match (r, is_hidden) {
                (Recipient::Id(id), false) => ("--recipient", id.clone()),
                (Recipient::Id(id), true) => ("--hidden-recipient", id.clone()),
                (Recipient::Key(k), false) => ("--recipient-file", files.add(k)?),
                (Recipient::Key(k), true) => ("--hidden-recipient-file", files.add(k)?),
            };
            args.extend([flag.to_string(), value]);
        }
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(gpg, &args, data, None);
    if out.stdout.is_empty() {
        return Err(out
            .detail
            .unwrap_or_else(|| i18n("gpg did not encrypt the message.")));
    }
    Ok(out.stdout)
}

/// Public keys written out for gpg's `--recipient-file`, which takes a
/// path: in a directory made for this run that only the user can enter,
/// removed with everything in it when the run is over.
#[derive(Default)]
struct KeyFiles {
    dir: Option<std::path::PathBuf>,
    count: usize,
}

impl KeyFiles {
    fn add(&mut self, key: &[u8]) -> Result<String, String> {
        let dir = match &self.dir {
            Some(d) => d.clone(),
            None => {
                let tag = crate::rng::from_alphabet(12, b"abcdefghijklmnopqrstuvwxyz0123456789")
                    .map_err(|e| e.to_string())?;
                let d =
                    std::env::temp_dir().join(format!("hylki-keys-{}-{tag}", std::process::id()));
                let mut builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder
                    .create(&d)
                    .map_err(|e| format!("{}: {e}", d.display()))?;
                self.dir = Some(d.clone());
                d
            }
        };
        self.count += 1;
        let path = dir.join(format!("{}.pgp", self.count));
        std::fs::write(&path, key).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path.to_string_lossy().into_owned())
    }
}

impl Drop for KeyFiles {
    fn drop(&mut self) {
        if let Some(d) = &self.dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
