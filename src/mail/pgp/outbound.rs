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

/// `data` encrypted (and, with `local_user`, signed) to every recipient
/// address, armoured. The sender is a recipient too, so the Sent copy stays
/// readable. Keys are used as found: a recipient's key that is in the
/// keyring but not vouched for still encrypts, as Thunderbird does.
///
/// `hidden` recipients (Bcc) go in with their key ids zeroed: every copy of
/// the message carries the same encrypted packet, so a Bcc key id written
/// out in it would tell the To and Cc who else could read it.
pub(crate) fn encrypt(
    gpg: &Gpg,
    data: &[u8],
    recipients: &[String],
    hidden: &[String],
    local_user: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut args: Vec<&str> = vec!["--armor", "--trust-model", "always", "--encrypt"];
    if let Some(u) = local_user {
        args.extend(["--sign", "--local-user", u]);
    }
    for r in recipients {
        args.extend(["--recipient", r.as_str()]);
    }
    for r in hidden {
        args.extend(["--hidden-recipient", r.as_str()]);
    }
    let out = run(gpg, &args, data, None);
    if out.stdout.is_empty() {
        return Err(out
            .detail
            .unwrap_or_else(|| i18n("gpg did not encrypt the message.")));
    }
    Ok(out.stdout)
}
