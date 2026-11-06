//! gpg's `--status-fd` lines and the verdict they add up to.

use super::keys::key_display;
use crate::i18n::{i18n, i18n_f};
use crate::models::{PgpSignature, PgpTrust};

/// The signature-related lines of a gpg run, distilled.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Outcome {
    pub(super) decryption_ok: bool,
    pub(super) no_seckey: Vec<String>,
    pub(super) nodata: bool,
    /// GOODSIG / BADSIG / EXPKEYSIG / REVKEYSIG / EXPSIG: (kind, key id, user id).
    pub(super) sig: Option<(String, String, String)>,
    /// ERRSIG's key id when the key is missing (NO_PUBKEY, or rc 9).
    pub(super) missing_key: Option<String>,
    pub(super) trust: Option<PgpTrust>,
    pub(super) fingerprint: Option<String>,
}

impl Outcome {
    /// A key the message names that this keyring lacks. A hidden recipient's
    /// key id is all zeros, which names nobody.
    pub(super) fn named_missing_key(&self) -> Option<&str> {
        self.no_seckey
            .iter()
            .map(String::as_str)
            .find(|k| k.chars().any(|c| c != '0'))
    }
}

/// Read gpg's status lines (`doc/DETAILS` in the GnuPG source).
pub(super) fn parse_status(lines: &[String]) -> Outcome {
    let mut o = Outcome::default();
    for line in lines {
        let mut it = line.splitn(2, ' ');
        let tag = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("").trim();
        match tag {
            "DECRYPTION_OKAY" => o.decryption_ok = true,
            "NO_SECKEY" => o.no_seckey.push(rest.to_string()),
            "NODATA" => o.nodata = true,
            "GOODSIG" | "BADSIG" | "EXPKEYSIG" | "REVKEYSIG" | "EXPSIG" => {
                let (key, uid) = rest.split_once(' ').unwrap_or((rest, ""));
                // A later line never downgrades a verdict already read for
                // the same signature; the first one is the one gpg meant.
                if o.sig.is_none() {
                    o.sig = Some((tag.to_string(), key.to_string(), uid.to_string()));
                }
            }
            "VALIDSIG" => {
                if let Some(fpr) = rest.split(' ').next() {
                    o.fingerprint = Some(fpr.to_string());
                }
            }
            "ERRSIG" => {
                // ERRSIG <keyid> <pkalgo> <hashalgo> <sigclass> <time> <rc> [<fpr>]
                let f: Vec<&str> = rest.split(' ').collect();
                if f.get(5) == Some(&"9") {
                    o.missing_key = f.first().map(|s| s.to_string());
                }
            }
            "NO_PUBKEY" => o.missing_key = Some(rest.to_string()),
            "TRUST_UNDEFINED" => o.trust = Some(PgpTrust::Unknown),
            "TRUST_NEVER" => o.trust = Some(PgpTrust::Never),
            "TRUST_MARGINAL" => o.trust = Some(PgpTrust::Marginal),
            "TRUST_FULLY" | "TRUST_ULTIMATE" => o.trust = Some(PgpTrust::Full),
            _ => {}
        }
    }
    o
}

/// The line that says how far the keyring trusts a signing key.
pub(super) fn trust_note(trust: PgpTrust, key_id: &str) -> String {
    let id = key_display(key_id);
    match trust {
        PgpTrust::Full => i18n_f("Signing key {id} is trusted in your keyring.", &[("id", &id)]),
        PgpTrust::Marginal => i18n_f("Signing key {id} is only marginally trusted in your keyring.", &[("id", &id)]),
        PgpTrust::Never => i18n_f("Signing key {id} is marked as not trusted in your keyring.", &[("id", &id)]),
        PgpTrust::Unknown => i18n_f(
            "Signing key {id} is in your keyring but not marked as trusted; check its fingerprint with the sender.",
            &[("id", &id)],
        ),
    }
}

/// The verdict on the signature, with its explanatory lines.
pub(super) fn signature_verdict(o: &Outcome, notes: &mut Vec<String>) -> PgpSignature {
    if let Some((kind, key, uid)) = &o.sig {
        let signer = if uid.is_empty() {
            key_display(key)
        } else {
            uid.clone()
        };
        let key_id = o.fingerprint.clone().unwrap_or_else(|| key.clone());
        return match kind.as_str() {
            "GOODSIG" => {
                let trust = o.trust.unwrap_or(PgpTrust::Unknown);
                notes.push(trust_note(trust, &key_id));
                // Compared with the From later, by `unwrap_message`, which
                // has the message and the keyring.
                PgpSignature::Good {
                    signer,
                    key_id,
                    trust,
                    from_mismatch: false,
                }
            }
            "BADSIG" => {
                notes.push(i18n_f(
                    "Signed with key {id}.",
                    &[("id", &key_display(&key_id))],
                ));
                PgpSignature::Bad { signer }
            }
            "EXPKEYSIG" => PgpSignature::ExpiredKey { signer },
            "REVKEYSIG" => PgpSignature::RevokedKey { signer },
            _ => PgpSignature::ExpiredSignature { signer },
        };
    }
    if let Some(k) = &o.missing_key {
        notes.push(i18n("Import the sender's public key to verify it."));
        return PgpSignature::NoKey { key_id: k.clone() };
    }
    PgpSignature::None
}
