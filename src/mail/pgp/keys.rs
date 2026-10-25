//! Key management (#133, second slice): what the settings page and the
//! reader's popover need, all of it gpg on the user's own keyring.

use super::{run, Gpg, Run};
use crate::i18n::i18n;

/// A key id or fingerprint in readable groups: the last sixteen hex digits,
/// four at a time.
pub(crate) fn key_display(id: &str) -> String {
    let hex: String = id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_uppercase();
    let tail: String = hex
        .chars()
        .rev()
        .take(16)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    groups_of_four(&tail)
}

/// Hex digits in groups of four, the way keys are read out.
fn groups_of_four(hex: &str) -> String {
    hex.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// How far the keyring vouches for a key (the validity column of
/// `--with-colons`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyValidity {
    Revoked,
    Expired,
    Unknown,
    Never,
    Marginal,
    Full,
    Ultimate,
}

impl KeyValidity {
    fn from_colon(c: &str) -> KeyValidity {
        match c.chars().next() {
            Some('r') => KeyValidity::Revoked,
            Some('e') => KeyValidity::Expired,
            Some('n') => KeyValidity::Never,
            Some('m') => KeyValidity::Marginal,
            Some('f') => KeyValidity::Full,
            Some('u') => KeyValidity::Ultimate,
            _ => KeyValidity::Unknown,
        }
    }

    /// Whether signatures by this key count as trusted.
    pub(crate) fn trusted(self) -> bool {
        matches!(self, KeyValidity::Full | KeyValidity::Ultimate)
    }

    pub(crate) fn label(self) -> String {
        match self {
            KeyValidity::Revoked => i18n("Revoked"),
            KeyValidity::Expired => i18n("Expired"),
            KeyValidity::Unknown => i18n("Not trusted yet"),
            KeyValidity::Never => i18n("Marked as not trusted"),
            KeyValidity::Marginal => i18n("Marginally trusted"),
            KeyValidity::Full => i18n("Trusted"),
            KeyValidity::Ultimate => i18n("Your own key"),
        }
    }
}

/// One key in the keyring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyInfo {
    pub(crate) fingerprint: String,
    /// The long key id (the fingerprint's last sixteen digits).
    pub(crate) key_id: String,
    /// User ids, the primary first.
    pub(crate) uids: Vec<String>,
    /// Whether the secret half is in the keyring (one of the user's own).
    pub(super) secret: bool,
    /// Whether the key (some subkey) can encrypt.
    pub(crate) can_encrypt: bool,
    /// Whether the key can sign.
    pub(crate) can_sign: bool,
    created: i64,
    pub(crate) expires: Option<i64>,
    pub(crate) validity: KeyValidity,
    pub(super) disabled: bool,
}

impl KeyInfo {
    pub(crate) fn primary_uid(&self) -> &str {
        self.uids.first().map(String::as_str).unwrap_or("")
    }

    /// The addresses in the user ids, lowercased.
    pub(crate) fn emails(&self) -> Vec<String> {
        self.uids
            .iter()
            .filter_map(|u| {
                let (_, addr) = crate::config::split_identity(u);
                let addr = addr.trim().to_ascii_lowercase();
                (!addr.is_empty()).then_some(addr)
            })
            .collect()
    }

    pub(super) fn matches_address(&self, addr: &str) -> bool {
        let addr = addr.trim().to_ascii_lowercase();
        self.emails().contains(&addr)
    }

    /// Whether the key can still be used at all.
    pub(crate) fn usable(&self) -> bool {
        !self.disabled
            && !matches!(self.validity, KeyValidity::Revoked | KeyValidity::Expired)
            && self.expires.is_none_or(|e| e > crate::datefmt::now())
    }

    /// Whether every word of `query` is in one of the key's user ids, its
    /// key id or its fingerprint, ignoring case (#432). Key ids are often
    /// written with a `0x` in front, and fingerprints in groups of four,
    /// which split into words that each match.
    pub(crate) fn matches_search(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {}",
            self.uids.join(" "),
            self.key_id,
            self.fingerprint
        )
        .to_lowercase();
        query.to_lowercase().split_whitespace().all(|word| {
            let word = word
                .strip_prefix("0x")
                .filter(|w| !w.is_empty())
                .unwrap_or(word);
            haystack.contains(word)
        })
    }

    /// The fingerprint in readable groups of four.
    pub(crate) fn fingerprint_display(&self) -> String {
        groups_of_four(&self.fingerprint)
    }
}

/// The keys in the keyring: the user's own (`secret`) or everyone's.
pub(crate) fn list_keys(gpg: &Gpg, secret: bool) -> Vec<KeyInfo> {
    let list = if secret {
        "--list-secret-keys"
    } else {
        "--list-keys"
    };
    let out = run(
        gpg,
        &[
            "--with-colons",
            "--with-fingerprint",
            "--fixed-list-mode",
            list,
        ],
        b"",
        None,
    );
    parse_colons(&String::from_utf8_lossy(&out.stdout), secret)
}

/// Read `--with-colons` output (`doc/DETAILS`): a `pub`/`sec` record opens
/// a key, its `fpr` names it, `uid` records follow, `sub`/`ssb` add
/// capabilities.
pub(super) fn parse_colons(text: &str, secret: bool) -> Vec<KeyInfo> {
    let mut keys: Vec<KeyInfo> = Vec::new();
    let mut expect_fpr = false;
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        match f.first().copied() {
            Some("pub") | Some("sec") => {
                let caps = f.get(11).copied().unwrap_or("");
                keys.push(KeyInfo {
                    fingerprint: String::new(),
                    key_id: f.get(4).copied().unwrap_or("").to_string(),
                    uids: Vec::new(),
                    secret,
                    can_encrypt: caps.contains(['e', 'E']),
                    can_sign: caps.contains(['s', 'S']),
                    created: f.get(5).and_then(|v| v.parse().ok()).unwrap_or(0),
                    expires: f
                        .get(6)
                        .and_then(|v| v.parse().ok())
                        .filter(|e: &i64| *e > 0),
                    validity: KeyValidity::from_colon(f.get(1).copied().unwrap_or("")),
                    disabled: caps.contains('D'),
                });
                expect_fpr = true;
            }
            Some("fpr") if expect_fpr => {
                if let Some(k) = keys.last_mut() {
                    k.fingerprint = f.get(9).copied().unwrap_or("").to_string();
                }
                expect_fpr = false;
            }
            Some("uid") => {
                if let Some(k) = keys.last_mut() {
                    let uid = f.get(9).copied().unwrap_or("");
                    // gpg writes the uid C-escaped (\x3a for ':').
                    let uid = uid.replace("\\x3a", ":");
                    let valid = f.get(1).copied().unwrap_or("");
                    if !valid.starts_with('r') {
                        k.uids.push(uid);
                    }
                }
            }
            Some("sub") | Some("ssb") => {
                if let Some(k) = keys.last_mut() {
                    let caps = f.get(11).copied().unwrap_or("");
                    let expired = f.get(1).copied().unwrap_or("").starts_with(['e', 'r']);
                    if !expired {
                        k.can_encrypt |= caps.contains('e');
                        k.can_sign |= caps.contains('s');
                    }
                }
                expect_fpr = false;
            }
            _ => {}
        }
    }
    keys.retain(|k| !k.fingerprint.is_empty());
    keys
}

/// The user's own usable key for an address, if any.
pub(crate) fn secret_key_for(gpg: &Gpg, addr: &str) -> Option<KeyInfo> {
    list_keys(gpg, true)
        .into_iter()
        .find(|k| k.usable() && k.can_sign && k.matches_address(addr))
}

/// A usable public key that can encrypt to an address, if any.
pub(crate) fn public_key_for(gpg: &Gpg, addr: &str) -> Option<KeyInfo> {
    list_keys(gpg, false)
        .into_iter()
        .find(|k| k.usable() && k.can_encrypt && k.matches_address(addr))
}

/// The key with this fingerprint, if the keyring holds it.
pub(crate) fn key_by_fingerprint(gpg: &Gpg, fpr: &str) -> Option<KeyInfo> {
    let fpr = fpr.to_ascii_uppercase();
    list_keys(gpg, false)
        .into_iter()
        .find(|k| k.fingerprint == fpr)
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ImportSummary {
    pub(crate) imported: u32,
    pub(super) unchanged: u32,
    /// The fingerprints the import touched.
    pub(crate) fingerprints: Vec<String>,
}

impl ImportSummary {
    /// Count another import into this one.
    fn add(&mut self, other: ImportSummary) {
        self.imported += other.imported;
        self.unchanged += other.unchanged;
        self.fingerprints.extend(other.fingerprints);
    }
}

pub(super) fn import_summary(out: &Run) -> ImportSummary {
    let mut s = ImportSummary::default();
    for line in &out.status_lines {
        // IMPORT_OK <reason> <fingerprint>: reason 0 = unchanged, else new
        // or updated.
        if let Some(rest) = line.strip_prefix("IMPORT_OK ") {
            let mut it = rest.split(' ');
            let reason: u32 = it.next().and_then(|r| r.parse().ok()).unwrap_or(0);
            if let Some(fpr) = it.next() {
                s.fingerprints.push(fpr.to_string());
            }
            if reason == 0 {
                s.unchanged += 1;
            } else {
                s.imported += 1;
            }
        }
    }
    s
}

/// Import keys (armoured or binary) into the keyring.
pub(crate) fn import_keys(gpg: &Gpg, bytes: &[u8]) -> Result<ImportSummary, String> {
    let out = run(gpg, &["--import"], bytes, None);
    let s = import_summary(&out);
    if s.fingerprints.is_empty() {
        return Err(out
            .detail
            .unwrap_or_else(|| i18n("No key was found in that file.")));
    }
    Ok(s)
}

/// A key's public half, armoured, for sending to someone.
pub(crate) fn export_public(gpg: &Gpg, fpr: &str) -> Result<Vec<u8>, String> {
    let out = run(gpg, &["--armor", "--export", fpr], b"", None);
    if out.stdout.is_empty() {
        return Err(out.detail.unwrap_or_else(|| i18n("Nothing to export.")));
    }
    Ok(out.stdout)
}

/// Make a new key pair for an identity: a signing primary with an
/// encryption subkey (`default default`, the pair the terminal command's
/// bare `rsa4096` does NOT give), protected by `passphrase` (empty = none).
/// The passphrase goes down a pipe, never a command line.
pub(crate) fn generate_key(
    gpg: &Gpg,
    name: &str,
    email: &str,
    expire: &str,
    passphrase: &str,
) -> Result<String, String> {
    let uid = if name.trim().is_empty() {
        email.trim().to_string()
    } else {
        format!("{} <{}>", name.trim(), email.trim())
    };
    let out = run(
        gpg,
        &[
            "--pinentry-mode",
            "loopback",
            "--passphrase-fd",
            "0",
            "--quick-gen-key",
            &uid,
            "default",
            "default",
            expire,
        ],
        format!("{passphrase}\n").as_bytes(),
        None,
    );
    for line in &out.status_lines {
        if let Some(rest) = line.strip_prefix("KEY_CREATED ") {
            if let Some(fpr) = rest.split(' ').nth(1) {
                return Ok(fpr.to_string());
            }
        }
    }
    Err(out
        .detail
        .unwrap_or_else(|| i18n("gpg did not create the key.")))
}

/// Look a key up by address (WKD, then the keyservers), and by key id
/// when the message named one, importing what is found.
pub(crate) fn fetch_key(
    gpg: &Gpg,
    addr: &str,
    key_id: Option<&str>,
) -> Result<ImportSummary, String> {
    let mut total = ImportSummary::default();
    let mut detail = None;
    if !addr.trim().is_empty() {
        let out = run(
            gpg,
            &[
                "--auto-key-locate",
                "clear,wkd,keyserver",
                "--locate-external-keys",
                addr.trim(),
            ],
            b"",
            None,
        );
        total.add(import_summary(&out));
        detail = out.detail;
    }
    if total.fingerprints.is_empty() {
        if let Some(id) = key_id.filter(|k| !k.is_empty()) {
            let out = run(gpg, &["--recv-keys", id], b"", None);
            total.add(import_summary(&out));
            if out.detail.is_some() {
                detail = out.detail;
            }
        }
    }
    if total.fingerprints.is_empty() {
        return Err(detail.unwrap_or_else(|| i18n("No key was found for that address.")));
    }
    Ok(total)
}

/// Vouch for a key: a local (non-exportable) signature with one of the
/// user's own keys, which is what makes gpg call it trusted.
pub(crate) fn trust_key(gpg: &Gpg, fpr: &str, signer: Option<&str>) -> Result<(), String> {
    let mut args = vec!["--yes"];
    if let Some(s) = signer {
        args.extend(["--local-user", s]);
    }
    args.extend(["--quick-lsign-key", fpr]);
    let out = run(gpg, &args, b"", None);
    // gpg says nothing on success; a failure has a line.
    let failed = out.status_lines.iter().any(|l| {
        l.starts_with("FAILURE") || l.starts_with("INV_SGNR") || l.starts_with("NO_SECKEY")
    });
    if failed {
        return Err(out
            .detail
            .unwrap_or_else(|| i18n("gpg could not sign the key.")));
    }
    Ok(())
}

/// Remove a key from the keyring; the secret half too when it is the
/// user's own.
pub(crate) fn delete_key(gpg: &Gpg, fpr: &str, secret: bool) -> Result<(), String> {
    let cmd = if secret {
        "--delete-secret-and-public-key"
    } else {
        "--delete-keys"
    };
    let out = run(gpg, &["--yes", cmd, fpr], b"", None);
    if key_by_fingerprint(gpg, fpr).is_some() {
        return Err(out
            .detail
            .unwrap_or_else(|| i18n("gpg could not delete the key.")));
    }
    Ok(())
}

/// One of the user's own keys by fingerprint, if usable for signing.
pub(crate) fn secret_key_by_fingerprint(gpg: &Gpg, fpr: &str) -> Option<KeyInfo> {
    list_keys(gpg, true)
        .into_iter()
        .find(|k| k.fingerprint.eq_ignore_ascii_case(fpr) && k.usable() && k.can_sign)
}
