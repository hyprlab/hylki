//! OpenPGP (#133), first slice: decrypt and verify what arrives, through the
//! user's own GnuPG — `gpg` on the path, the keyring in `~/.gnupg`, the agent
//! (and its pinentry) for passphrases. Hylki never sees a secret key and never
//! writes anything decrypted to disk: the worker renders a decrypted message
//! straight to the reader and leaves the cache alone.
//!
//! What is recognised: PGP/MIME (RFC 3156) `multipart/encrypted` and
//! `multipart/signed`, and the older inline forms — an armoured
//! `-----BEGIN PGP MESSAGE-----` or a clear-signed block in the text body.
//! Signing and encrypting outgoing mail, and key management, are later slices.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::i18n::{i18n, i18n_f};
use crate::models::{PgpSignature, PgpStatus, PgpTrust};

/// A GnuPG to talk to: the system one by default, or one with its own home
/// directory (tests build a throwaway keyring).
#[derive(Debug, Clone, Default)]
pub struct Gpg {
    /// `GNUPGHOME` for the child process; `None` = the user's own.
    pub home: Option<PathBuf>,
}

impl Gpg {
    /// The user's GnuPG (honouring `HYLKI_GNUPGHOME`, for trying an
    /// alternative keyring without touching the real one).
    pub fn system() -> Gpg {
        Gpg { home: std::env::var_os("HYLKI_GNUPGHOME").map(PathBuf::from) }
    }

    fn command(&self) -> Command {
        let mut c = Command::new("gpg");
        // `--batch` keeps gpg off the terminal; the agent still asks for a
        // passphrase through pinentry, which is exactly the prompt we want.
        c.args(["--batch", "--no-tty", "--status-fd", "2", "--exit-on-status-write-error"]);
        if let Some(h) = &self.home {
            c.env("GNUPGHOME", h);
        }
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        c
    }
}

/// Whether a `gpg` answers at all. Checked once per process.
pub fn available() -> bool {
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

/// The OpenPGP structure a raw message carries, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// PGP/MIME `multipart/encrypted`: the armoured ciphertext (part two).
    Encrypted { armored: Vec<u8> },
    /// PGP/MIME `multipart/signed`: the signed entity's exact bytes and the
    /// detached signature.
    Signed { data: Vec<u8>, signature: Vec<u8> },
    /// An armoured message inside a text body.
    InlineEncrypted { armored: Vec<u8> },
    /// A clear-signed block inside a text body.
    InlineSigned { text: Vec<u8> },
}

impl Shape {
    pub fn is_encrypted(&self) -> bool {
        matches!(self, Shape::Encrypted { .. } | Shape::InlineEncrypted { .. })
    }
}

/// Whether the message is OpenPGP-encrypted (either form). Cheap: no gpg.
/// The prefetcher asks this so it never triggers a passphrase prompt on its
/// own; only an open decrypts.
pub fn is_encrypted(raw: &[u8]) -> bool {
    detect(raw).is_some_and(|s| s.is_encrypted())
}

/// Find the OpenPGP structure in a raw message.
pub fn detect(raw: &[u8]) -> Option<Shape> {
    use mail_parser::{MessageParser, MimeHeaders, PartType};
    let parsed = MessageParser::default().parse(raw)?;
    let root = parsed.root_part();
    let ctype = parsed.content_type();
    let (ct_type, ct_sub, protocol, boundary) = match ctype {
        Some(ct) => (
            ct.ctype().to_ascii_lowercase(),
            ct.subtype().map(|s| s.to_ascii_lowercase()).unwrap_or_default(),
            ct.attribute("protocol").map(|p| p.to_ascii_lowercase()).unwrap_or_default(),
            ct.attribute("boundary").map(str::to_string),
        ),
        None => (String::new(), String::new(), String::new(), None),
    };
    if ct_type == "multipart" {
        let PartType::Multipart(children) = &root.body else {
            return None;
        };
        if ct_sub == "encrypted" && protocol == "application/pgp-encrypted" {
            // Part one is the version stub; part two carries the ciphertext.
            // Take the first part that looks like an armoured message rather
            // than trusting positions.
            for id in children {
                let part = parsed.part(*id)?;
                let text = part_text(part);
                if text.contains("-----BEGIN PGP MESSAGE-----") {
                    return Some(Shape::Encrypted { armored: text.into_bytes() });
                }
            }
            return None;
        }
        if ct_sub == "signed" && protocol == "application/pgp-signature" {
            let (first, second) = (children.first()?, children.get(1)?);
            let first = parsed.part(*first)?;
            let sig = parsed.part(*second)?;
            let boundary = boundary?;
            let data = signed_bytes(raw, first.offset_header, &boundary)?;
            let signature = part_text(sig).into_bytes();
            if !signature.contains_str("-----BEGIN PGP SIGNATURE-----") {
                return None;
            }
            return Some(Shape::Signed { data, signature });
        }
    }
    // Inline forms: the first text body.
    let text = parsed.body_text(0)?;
    if text.contains("-----BEGIN PGP MESSAGE-----") && text.contains("-----END PGP MESSAGE-----") {
        return Some(Shape::InlineEncrypted { armored: armored_block(&text, "MESSAGE").into_bytes() });
    }
    if text.contains("-----BEGIN PGP SIGNED MESSAGE-----") && text.contains("-----END PGP SIGNATURE-----") {
        return Some(Shape::InlineSigned { text: text.into_owned().into_bytes() });
    }
    None
}

trait ContainsStr {
    fn contains_str(&self, needle: &str) -> bool;
}

impl ContainsStr for Vec<u8> {
    fn contains_str(&self, needle: &str) -> bool {
        self.windows(needle.len()).any(|w| w == needle.as_bytes())
    }
}

/// A part's decoded text (an armoured block is text whatever it is labelled).
fn part_text(part: &mail_parser::MessagePart) -> String {
    use mail_parser::PartType;
    match &part.body {
        PartType::Text(t) | PartType::Html(t) => t.to_string(),
        PartType::Binary(b) | PartType::InlineBinary(b) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    }
}

/// The armoured block between the BEGIN and END lines, inclusive.
fn armored_block(text: &str, kind: &str) -> String {
    let begin = format!("-----BEGIN PGP {kind}-----");
    let end = format!("-----END PGP {kind}-----");
    let (Some(b), Some(e)) = (text.find(&begin), text.find(&end)) else {
        return text.to_string();
    };
    text[b..e + end.len()].to_string()
}

/// The exact bytes the signature covers (RFC 3156 §5): the signed part from
/// its first header up to, but not including, the CRLF that precedes the
/// boundary delimiter. Found on the raw bytes, not through the parser, so
/// nothing is re-encoded on the way.
fn signed_bytes(raw: &[u8], start: usize, boundary: &str) -> Option<Vec<u8>> {
    if start >= raw.len() {
        return None;
    }
    let crlf = format!("\r\n--{boundary}");
    let lf = format!("\n--{boundary}");
    let rest = &raw[start..];
    let end = find(rest, crlf.as_bytes()).or_else(|| find(rest, lf.as_bytes()))?;
    Some(rest[..end].to_vec())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The outcome of handling a message: what to render, and the verdict.
#[derive(Debug, Clone)]
pub struct Unwrapped {
    /// The MIME entity to render and take attachments from: the decrypted
    /// message, or the signed part. `None` when there is nothing readable
    /// (undecryptable), in which case the reader shows the verdict instead.
    pub inner: Option<Vec<u8>>,
    pub status: PgpStatus,
}

/// Decrypt and/or verify a message, whatever form it takes. `None` when the
/// message carries no OpenPGP structure at all.
pub fn unwrap_message(raw: &[u8], gpg: &Gpg) -> Option<Unwrapped> {
    let shape = detect(raw)?;
    let mut unwrapped = unwrap_shape(shape, raw, gpg);
    unwrapped.status.sender_addr = from_address(raw);
    unwrapped.status.autocrypt = autocrypt_key(raw).map(|k| crate::oauth::base64_encode(&k));
    Some(unwrapped)
}

/// The From address of a raw message, lowercased ("" when it has none).
fn from_address(raw: &[u8]) -> String {
    use mail_parser::MessageParser;
    MessageParser::default()
        .parse(raw)
        .and_then(|m| m.from().and_then(|f| f.first()).and_then(|a| a.address().map(|s| s.to_ascii_lowercase())))
        .unwrap_or_default()
}

fn unwrap_shape(shape: Shape, _raw: &[u8], gpg: &Gpg) -> Unwrapped {
    if !available() {
        let status = PgpStatus {
            encrypted: shape.is_encrypted(),
            decrypted: false,
            signature: PgpSignature::None,
            notes: vec![i18n("GnuPG (gpg) is not installed, so this message cannot be read.")],
            ..Default::default()
        };
        // A signed message is still readable without gpg.
        let inner = match &shape {
            Shape::Signed { data, .. } => Some(data.clone()),
            Shape::InlineSigned { text } => Some(text_entity(text)),
            _ => None,
        };
        return Unwrapped { inner, status };
    }
    match shape {
        Shape::Encrypted { armored } | Shape::InlineEncrypted { armored } => {
            let out = run(gpg, &["--decrypt"], &armored, None);
            let outcome = parse_status(&out.status_lines);
            let decrypted = outcome.decryption_ok && !out.stdout.is_empty();
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            if decrypted {
                notes.insert(0, i18n("Decrypted with your key."));
            } else if let Some(k) = outcome.no_seckey.first() {
                notes.insert(0, i18n_f("Encrypted to key {id}, which is not in your keyring.", &[("id", &key_display(k))]));
            } else if outcome.nodata {
                notes.insert(0, i18n("The encrypted block is damaged or not OpenPGP data."));
            } else if let Some(d) = out.detail.clone() {
                notes.insert(0, d);
            }
            let inner = decrypted.then(|| {
                // PGP/MIME wraps a full MIME entity; inline PGP is bare text.
                if looks_like_mime(&out.stdout) {
                    out.stdout.clone()
                } else {
                    text_entity(&out.stdout)
                }
            });
            Unwrapped { inner, status: PgpStatus { encrypted: true, decrypted, signature, notes, ..Default::default() } }
        }
        Shape::Signed { data, signature } => {
            let out = run(gpg, &["--verify"], &data, Some(&signature));
            let outcome = parse_status(&out.status_lines);
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            if matches!(signature, PgpSignature::None) {
                if let Some(d) = out.detail.clone() {
                    notes.push(d);
                }
            }
            Unwrapped {
                inner: Some(data),
                status: PgpStatus { encrypted: false, decrypted: false, signature, notes, ..Default::default() },
            }
        }
        Shape::InlineSigned { text } => {
            // `--decrypt` on a clear-signed block verifies it and prints the
            // signed text, dearmoured.
            let out = run(gpg, &["--decrypt"], &text, None);
            let outcome = parse_status(&out.status_lines);
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            let inner = if out.stdout.is_empty() { text_entity(&text) } else { text_entity(&out.stdout) };
            Unwrapped {
                inner: Some(inner),
                status: PgpStatus { encrypted: false, decrypted: false, signature, notes, ..Default::default() },
            }
        }
    }
}

/// Whether decrypted bytes are a MIME entity (headers first) rather than
/// bare text.
fn looks_like_mime(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let first = head.lines().next().unwrap_or("");
    let lower = first.to_ascii_lowercase();
    lower.starts_with("content-type:")
        || lower.starts_with("content-transfer-encoding:")
        || lower.starts_with("content-disposition:")
        || lower.starts_with("mime-version:")
}

/// Bare text as a `text/plain` entity the renderer understands.
fn text_entity(text: &[u8]) -> Vec<u8> {
    let mut out = b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n".to_vec();
    out.extend_from_slice(text);
    out
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
            return Run { detail: Some(format!("gpg: {e}")), ..Default::default() };
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
        return Run { detail: Some("gpg: no output".into()), ..Default::default() };
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
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
    Run { stdout: output.stdout, status_lines, detail }
}

/// The signature-related lines of a gpg run, distilled.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    decryption_ok: bool,
    no_seckey: Vec<String>,
    nodata: bool,
    /// GOODSIG / BADSIG / EXPKEYSIG / REVKEYSIG / EXPSIG: (kind, key id, user id).
    sig: Option<(String, String, String)>,
    /// ERRSIG's key id when the key is missing (NO_PUBKEY, or rc 9).
    missing_key: Option<String>,
    trust: Option<PgpTrust>,
    fingerprint: Option<String>,
}

/// Read gpg's status lines (`doc/DETAILS` in the GnuPG source).
fn parse_status(lines: &[String]) -> Outcome {
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

/// The verdict on the signature, with its explanatory lines.
fn signature_verdict(o: &Outcome, notes: &mut Vec<String>) -> PgpSignature {
    if let Some((kind, key, uid)) = &o.sig {
        let signer = if uid.is_empty() { key_display(key) } else { uid.clone() };
        let key_id = o.fingerprint.clone().unwrap_or_else(|| key.clone());
        return match kind.as_str() {
            "GOODSIG" => {
                let trust = o.trust.unwrap_or(PgpTrust::Unknown);
                notes.push(match trust {
                    PgpTrust::Full => i18n_f("Signing key {id} is trusted in your keyring.", &[("id", &key_display(&key_id))]),
                    PgpTrust::Marginal => i18n_f("Signing key {id} is only marginally trusted in your keyring.", &[("id", &key_display(&key_id))]),
                    PgpTrust::Never => i18n_f("Signing key {id} is marked as not trusted in your keyring.", &[("id", &key_display(&key_id))]),
                    PgpTrust::Unknown => i18n_f(
                        "Signing key {id} is in your keyring but not marked as trusted; check its fingerprint with the sender.",
                        &[("id", &key_display(&key_id))],
                    ),
                });
                PgpSignature::Good { signer, key_id, trust }
            }
            "BADSIG" => {
                notes.push(i18n_f("Signed with key {id}.", &[("id", &key_display(&key_id))]));
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

/// A key id or fingerprint in readable groups: the last sixteen hex digits,
/// four at a time.
pub fn key_display(id: &str) -> String {
    let hex: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_uppercase();
    let tail: String = hex.chars().rev().take(16).collect::<Vec<_>>().into_iter().rev().collect();
    tail.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Key management (#133, second slice): what the settings page and the
// reader's popover need, all of it gpg on the user's own keyring.

/// How far the keyring vouches for a key (the validity column of
/// `--with-colons`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyValidity {
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
    pub fn trusted(self) -> bool {
        matches!(self, KeyValidity::Full | KeyValidity::Ultimate)
    }

    pub fn label(self) -> String {
        i18n(match self {
            KeyValidity::Revoked => "Revoked",
            KeyValidity::Expired => "Expired",
            KeyValidity::Unknown => "Not trusted yet",
            KeyValidity::Never => "Marked as not trusted",
            KeyValidity::Marginal => "Marginally trusted",
            KeyValidity::Full => "Trusted",
            KeyValidity::Ultimate => "Your own key",
        })
    }
}

/// One key in the keyring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    pub fingerprint: String,
    /// The long key id (the fingerprint's last sixteen digits).
    pub key_id: String,
    /// User ids, the primary first.
    pub uids: Vec<String>,
    /// Whether the secret half is in the keyring (one of the user's own).
    pub secret: bool,
    /// Whether the key (some subkey) can encrypt.
    pub can_encrypt: bool,
    /// Whether the key can sign.
    pub can_sign: bool,
    pub created: i64,
    pub expires: Option<i64>,
    pub validity: KeyValidity,
    pub disabled: bool,
}

impl KeyInfo {
    pub fn primary_uid(&self) -> &str {
        self.uids.first().map(String::as_str).unwrap_or("")
    }

    /// The addresses in the user ids, lowercased.
    pub fn emails(&self) -> Vec<String> {
        self.uids
            .iter()
            .filter_map(|u| {
                let (_, addr) = crate::config::split_identity(u);
                let addr = addr.trim().to_ascii_lowercase();
                (!addr.is_empty()).then_some(addr)
            })
            .collect()
    }

    pub fn matches_address(&self, addr: &str) -> bool {
        let addr = addr.trim().to_ascii_lowercase();
        self.emails().iter().any(|e| *e == addr)
    }

    /// Whether the key can still be used at all.
    pub fn usable(&self) -> bool {
        !self.disabled
            && !matches!(self.validity, KeyValidity::Revoked | KeyValidity::Expired)
            && self.expires.is_none_or(|e| e > now_secs())
    }

    /// The fingerprint in readable groups of four.
    pub fn fingerprint_display(&self) -> String {
        self.fingerprint
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The keys in the keyring: the user's own (`secret`) or everyone's.
pub fn list_keys(gpg: &Gpg, secret: bool) -> Vec<KeyInfo> {
    let list = if secret { "--list-secret-keys" } else { "--list-keys" };
    let out = run(gpg, &["--with-colons", "--with-fingerprint", "--fixed-list-mode", list], b"", None);
    parse_colons(&String::from_utf8_lossy(&out.stdout), secret)
}

/// Read `--with-colons` output (`doc/DETAILS`): a `pub`/`sec` record opens
/// a key, its `fpr` names it, `uid` records follow, `sub`/`ssb` add
/// capabilities.
fn parse_colons(text: &str, secret: bool) -> Vec<KeyInfo> {
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
                    expires: f.get(6).and_then(|v| v.parse().ok()).filter(|e: &i64| *e > 0),
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
pub fn secret_key_for(gpg: &Gpg, addr: &str) -> Option<KeyInfo> {
    list_keys(gpg, true)
        .into_iter()
        .find(|k| k.usable() && k.can_sign && k.matches_address(addr))
}

/// A usable public key that can encrypt to an address, if any.
pub fn public_key_for(gpg: &Gpg, addr: &str) -> Option<KeyInfo> {
    list_keys(gpg, false)
        .into_iter()
        .find(|k| k.usable() && k.can_encrypt && k.matches_address(addr))
}

/// The key with this fingerprint, if the keyring holds it.
pub fn key_by_fingerprint(gpg: &Gpg, fpr: &str) -> Option<KeyInfo> {
    let fpr = fpr.to_ascii_uppercase();
    list_keys(gpg, false).into_iter().find(|k| k.fingerprint == fpr)
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub imported: u32,
    pub unchanged: u32,
    /// The fingerprints the import touched.
    pub fingerprints: Vec<String>,
}

fn import_summary(out: &Run) -> ImportSummary {
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
pub fn import_keys(gpg: &Gpg, bytes: &[u8]) -> Result<ImportSummary, String> {
    let out = run(gpg, &["--import"], bytes, None);
    let s = import_summary(&out);
    if s.fingerprints.is_empty() {
        return Err(out.detail.unwrap_or_else(|| i18n("No key was found in that file.")));
    }
    Ok(s)
}

/// A key's public half, armoured, for sending to someone.
pub fn export_public(gpg: &Gpg, fpr: &str) -> Result<Vec<u8>, String> {
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
pub fn generate_key(gpg: &Gpg, name: &str, email: &str, expire: &str, passphrase: &str) -> Result<String, String> {
    let uid = if name.trim().is_empty() { email.trim().to_string() } else { format!("{} <{}>", name.trim(), email.trim()) };
    let out = run(
        gpg,
        &["--pinentry-mode", "loopback", "--passphrase-fd", "0", "--quick-gen-key", &uid, "default", "default", expire],
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
    Err(out.detail.unwrap_or_else(|| i18n("gpg did not create the key.")))
}

/// Look a key up by address (WKD, then the keyservers), and by key id
/// when the message named one, importing what is found.
pub fn fetch_key(gpg: &Gpg, addr: &str, key_id: Option<&str>) -> Result<ImportSummary, String> {
    let mut total = ImportSummary::default();
    let mut detail = None;
    if !addr.trim().is_empty() {
        let out = run(gpg, &["--auto-key-locate", "clear,wkd,keyserver", "--locate-external-keys", addr.trim()], b"", None);
        let s = import_summary(&out);
        total.imported += s.imported;
        total.unchanged += s.unchanged;
        total.fingerprints.extend(s.fingerprints);
        detail = out.detail;
    }
    if total.fingerprints.is_empty() {
        if let Some(id) = key_id.filter(|k| !k.is_empty()) {
            let out = run(gpg, &["--recv-keys", id], b"", None);
            let s = import_summary(&out);
            total.imported += s.imported;
            total.unchanged += s.unchanged;
            total.fingerprints.extend(s.fingerprints);
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
pub fn trust_key(gpg: &Gpg, fpr: &str, signer: Option<&str>) -> Result<(), String> {
    let mut args = vec!["--yes"];
    if let Some(s) = signer {
        args.extend(["--local-user", s]);
    }
    args.extend(["--quick-lsign-key", fpr]);
    let out = run(gpg, &args, b"", None);
    // gpg says nothing on success; a failure has a line.
    let failed = out
        .status_lines
        .iter()
        .any(|l| l.starts_with("FAILURE") || l.starts_with("INV_SGNR") || l.starts_with("NO_SECKEY"));
    if failed {
        return Err(out.detail.unwrap_or_else(|| i18n("gpg could not sign the key.")));
    }
    Ok(())
}

/// Remove a key from the keyring; the secret half too when it is the
/// user's own.
pub fn delete_key(gpg: &Gpg, fpr: &str, secret: bool) -> Result<(), String> {
    let cmd = if secret { "--delete-secret-and-public-key" } else { "--delete-keys" };
    let out = run(gpg, &["--yes", cmd, fpr], b"", None);
    if key_by_fingerprint(gpg, fpr).is_some() {
        return Err(out.detail.unwrap_or_else(|| i18n("gpg could not delete the key.")));
    }
    Ok(())
}

/// A detached, armoured signature over `data` by `local_user`, with the
/// `micalg` value RFC 3156 wants beside it ("pgp-sha256"), read from gpg's
/// SIG_CREATED line.
pub fn sign_detached(gpg: &Gpg, data: &[u8], local_user: &str) -> Result<(Vec<u8>, String), String> {
    let out = run(gpg, &["--armor", "--local-user", local_user, "--detach-sign"], data, None);
    if out.stdout.is_empty() {
        return Err(out.detail.unwrap_or_else(|| i18n("gpg did not sign the message.")));
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

/// One of the user's own keys by fingerprint, if usable for signing.
pub fn secret_key_by_fingerprint(gpg: &Gpg, fpr: &str) -> Option<KeyInfo> {
    list_keys(gpg, true)
        .into_iter()
        .find(|k| k.fingerprint.eq_ignore_ascii_case(fpr) && k.usable() && k.can_sign)
}

/// `data` encrypted (and, with `local_user`, signed) to every recipient
/// address, armoured. The sender is a recipient too, so the Sent copy stays
/// readable. Keys are used as found: a recipient's key that is in the
/// keyring but not vouched for still encrypts, as Thunderbird does.
pub fn encrypt(gpg: &Gpg, data: &[u8], recipients: &[String], local_user: Option<&str>) -> Result<Vec<u8>, String> {
    let mut args: Vec<&str> = vec!["--armor", "--trust-model", "always", "--encrypt"];
    if let Some(u) = local_user {
        args.extend(["--sign", "--local-user", u]);
    }
    for r in recipients {
        args.extend(["--recipient", r.as_str()]);
    }
    let out = run(gpg, &args, data, None);
    if out.stdout.is_empty() {
        return Err(out.detail.unwrap_or_else(|| i18n("gpg did not encrypt the message.")));
    }
    Ok(out.stdout)
}

/// The sender's key carried in an Autocrypt header (Level 1: `addr=…;
/// keydata=<base64>`), decoded, if the message has one.
pub fn autocrypt_key(raw: &[u8]) -> Option<Vec<u8>> {
    use mail_parser::MessageParser;
    let parsed = MessageParser::default().parse(raw)?;
    let header = parsed.header_raw("Autocrypt")?;
    let mut keydata = None;
    for attr in header.split(';') {
        let attr = attr.trim();
        if let Some(v) = attr.strip_prefix("keydata=") {
            keydata = Some(v.split_whitespace().collect::<String>());
        }
    }
    let keydata = keydata?;
    crate::oauth::base64_decode(&keydata).filter(|b| !b.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn status_lines_distil_to_an_outcome() {
        let o = parse_status(&lines(&[
            "ENC_TO 1234567890ABCDEF 1 0",
            "DECRYPTION_OKAY",
            "GOODSIG 1234567890ABCDEF Ada Lovelace <ada@example.org>",
            "VALIDSIG 0123456789ABCDEF0123456789ABCDEF01234567 2026-09-07 1 0 4 0 1 8 00 0123456789ABCDEF0123456789ABCDEF01234567",
            "TRUST_ULTIMATE 0 pgp",
        ]));
        assert!(o.decryption_ok);
        assert_eq!(o.sig.as_ref().unwrap().0, "GOODSIG");
        assert_eq!(o.sig.as_ref().unwrap().2, "Ada Lovelace <ada@example.org>");
        assert_eq!(o.trust, Some(PgpTrust::Full));
        assert_eq!(o.fingerprint.as_deref(), Some("0123456789ABCDEF0123456789ABCDEF01234567"));

        let o = parse_status(&lines(&["ERRSIG 1234567890ABCDEF 1 8 00 1757000000 9 -", "NO_PUBKEY 1234567890ABCDEF"]));
        assert_eq!(o.missing_key.as_deref(), Some("1234567890ABCDEF"));
        assert!(o.sig.is_none());

        let o = parse_status(&lines(&["ENC_TO AAAA 1 0", "NO_SECKEY AAAA", "DECRYPTION_FAILED"]));
        assert!(!o.decryption_ok);
        assert_eq!(o.no_seckey, vec!["AAAA".to_string()]);
    }

    #[test]
    fn verdicts_follow_the_status() {
        let mut notes = Vec::new();
        let o = parse_status(&lines(&["GOODSIG AA Ada <a@b.c>", "TRUST_UNDEFINED 0 pgp"]));
        match signature_verdict(&o, &mut notes) {
            PgpSignature::Good { signer, trust, .. } => {
                assert_eq!(signer, "Ada <a@b.c>");
                assert_eq!(trust, PgpTrust::Unknown);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(notes.len(), 1);
        let o = parse_status(&lines(&["BADSIG AA Ada <a@b.c>"]));
        assert!(matches!(signature_verdict(&o, &mut notes), PgpSignature::Bad { .. }));
        let o = parse_status(&lines(&["EXPKEYSIG AA Ada <a@b.c>"]));
        assert!(matches!(signature_verdict(&o, &mut notes), PgpSignature::ExpiredKey { .. }));
        let o = parse_status(&lines(&["NO_PUBKEY AA"]));
        assert!(matches!(signature_verdict(&o, &mut notes), PgpSignature::NoKey { .. }));
        assert!(matches!(signature_verdict(&Outcome::default(), &mut notes), PgpSignature::None));
    }

    #[test]
    fn key_ids_read_in_groups_of_four() {
        assert_eq!(key_display("0123456789ABCDEF0123456789abcdef01234567"), "89AB CDEF 0123 4567");
        assert_eq!(key_display("1234567890ABCDEF"), "1234 5678 90AB CDEF");
    }

    const SIGNED: &str = "From: a@b.c\r\nTo: d@e.f\r\nSubject: hi\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=\"bnd\"\r\n\r\n\
--bnd\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello signed\r\nsecond line\r\n\
--bnd\r\nContent-Type: application/pgp-signature; name=\"signature.asc\"\r\n\r\n\
-----BEGIN PGP SIGNATURE-----\r\n\r\nabc\r\n-----END PGP SIGNATURE-----\r\n--bnd--\r\n";

    /// The signed bytes run from the part's first header to just before the
    /// CRLF that precedes the boundary — exactly what the signer hashed.
    #[test]
    fn detects_pgp_mime_signed_and_takes_exact_bytes() {
        match detect(SIGNED.as_bytes()) {
            Some(Shape::Signed { data, signature }) => {
                assert_eq!(
                    String::from_utf8(data).unwrap(),
                    "Content-Type: text/plain; charset=utf-8\r\n\r\nhello signed\r\nsecond line"
                );
                assert!(String::from_utf8(signature).unwrap().starts_with("-----BEGIN PGP SIGNATURE-----"));
            }
            other => panic!("{other:?}"),
        }
    }

    const ENCRYPTED: &str = "From: a@b.c\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n\
--enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
--enc\r\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\r\n\r\n\
-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----\r\n--enc--\r\n";

    #[test]
    fn detects_pgp_mime_encrypted_and_inline_forms() {
        match detect(ENCRYPTED.as_bytes()) {
            Some(Shape::Encrypted { armored }) => {
                let a = String::from_utf8(armored).unwrap();
                assert!(a.starts_with("-----BEGIN PGP MESSAGE-----"), "{a}");
            }
            other => panic!("{other:?}"),
        }
        assert!(is_encrypted(ENCRYPTED.as_bytes()));
        assert!(!is_encrypted(SIGNED.as_bytes()));

        let inline = "From: a@b.c\r\nContent-Type: text/plain\r\n\r\nSee below\r\n\
-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----\r\n";
        match detect(inline.as_bytes()) {
            Some(Shape::InlineEncrypted { armored }) => {
                assert_eq!(String::from_utf8(armored).unwrap(), "-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----");
            }
            other => panic!("{other:?}"),
        }
        let clear = "From: a@b.c\r\nContent-Type: text/plain\r\n\r\n-----BEGIN PGP SIGNED MESSAGE-----\r\nHash: SHA256\r\n\r\nhi\r\n-----BEGIN PGP SIGNATURE-----\r\n\r\nabc\r\n-----END PGP SIGNATURE-----\r\n";
        assert!(matches!(detect(clear.as_bytes()), Some(Shape::InlineSigned { .. })));
        assert_eq!(detect(b"From: a@b.c\r\nContent-Type: text/plain\r\n\r\nplain mail\r\n"), None);
        // A plain message that merely quotes the markers is not a signed one.
        let html_only = "From: a@b.c\r\nContent-Type: text/html\r\n\r\n<p>x</p>\r\n";
        assert_eq!(detect(html_only.as_bytes()), None);
    }

    #[test]
    fn mime_versus_bare_text_is_told_by_the_first_line() {
        assert!(looks_like_mime(b"Content-Type: text/plain\r\n\r\nx"));
        assert!(looks_like_mime(b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n"));
        assert!(!looks_like_mime(b"Hello there\r\n"));
        let e = text_entity(b"hi");
        assert!(e.starts_with(b"Content-Type: text/plain"));
    }

    #[test]
    fn colon_listing_becomes_keys() {
        let text = "\
tru::1:1757000000:0:3:1:5\n\
pub:u:255:22:7949BE459D0D7AF2:1750000000:0::u:::scESC::::::23::0:\n\
fpr:::::::::79277AB4A01A00F9574FD84E7949BE459D0D7AF2:\n\
uid:u::::1750000000::ABCDEF::jason@hyprlab.co <jason@hyprlab.co>::::::::::0:\n\
sub:u:255:18:94D25AE427F6BE67:1750000000:0:::::e::::::23:\n\
fpr:::::::::1111111111111111111111111194D25AE427F6BE67:\n\
pub:-:4096:1:51300FD844417A2E:1757000000:1820000000::-:::scSC::::::23::0:\n\
fpr:::::::::51300FD844417A2EF1828831DC8357B877CD8F05:\n\
uid:-::::1757000000::ABCDEF::Jason Martin <jasonjmartin@me.com>::::::::::0:\n\
uid:r::::1757000000::ABCDEF::Old Name <old@example.org>::::::::::0:\n";
        let keys = parse_colons(text, false);
        assert_eq!(keys.len(), 2);
        let k = &keys[0];
        assert_eq!(k.fingerprint, "79277AB4A01A00F9574FD84E7949BE459D0D7AF2");
        assert_eq!(k.key_id, "7949BE459D0D7AF2");
        assert!(k.can_encrypt && k.can_sign);
        assert_eq!(k.validity, KeyValidity::Ultimate);
        assert!(k.matches_address("Jason@Hyprlab.co"));
        assert_eq!(k.fingerprint_display(), "7927 7AB4 A01A 00F9 574F D84E 7949 BE45 9D0D 7AF2");
        let k = &keys[1];
        assert!(!k.can_encrypt, "no encryption subkey");
        assert_eq!(k.expires, Some(1820000000));
        assert_eq!(k.validity, KeyValidity::Unknown);
        assert_eq!(k.uids, vec!["Jason Martin <jasonjmartin@me.com>"], "revoked uid dropped");
        assert_eq!(k.emails(), vec!["jasonjmartin@me.com"]);
    }

    #[test]
    fn import_status_is_summarised() {
        let out = Run {
            status_lines: lines(&["IMPORT_OK 1 AAAA", "IMPORT_OK 0 BBBB", "IMPORT_RES 2 0 1 0 0 0 0 0 0 0 0 0 0 0 0"]),
            ..Default::default()
        };
        let s = import_summary(&out);
        assert_eq!(s.imported, 1);
        assert_eq!(s.unchanged, 1);
        assert_eq!(s.fingerprints, vec!["AAAA", "BBBB"]);
    }

    #[test]
    fn autocrypt_header_yields_the_key() {
        let key = b"\x98\x01\x02binarykey";
        let b64 = crate::oauth::base64_encode(key);
        let raw = format!(
            "From: a@b.c\r\nAutocrypt: addr=a@b.c; prefer-encrypt=mutual;\r\n keydata={}\r\nContent-Type: text/plain\r\n\r\nhi\r\n",
            b64
        );
        assert_eq!(autocrypt_key(raw.as_bytes()).as_deref(), Some(&key[..]));
        assert_eq!(autocrypt_key(b"From: a@b.c\r\n\r\nhi"), None);
    }

    /// The end-to-end path against a real gpg with a throwaway keyring:
    /// encrypt-and-sign a MIME entity, then watch it come back decrypted with
    /// the signature verified. Skipped where gpg is missing.
    #[test]
    fn round_trip_through_a_throwaway_keyring() {
        if !available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let home = std::env::temp_dir().join(format!("hylki-gpg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let gpg = Gpg { home: Some(home.clone()) };
        let status = |args: &[&str], input: &[u8]| -> Run { run(&gpg, args, input, None) };
        let gen = status(&["--passphrase", "", "--pinentry-mode", "loopback", "--quick-gen-key", "Test Hylki <test@hylki.invalid>", "default", "default", "never"], b"");
        assert!(gen.status_lines.iter().any(|l| l.starts_with("KEY_CREATED")), "{gen:?}");
        let entity = b"Content-Type: text/plain; charset=utf-8\r\n\r\nsecret hello\r\n";
        let enc = status(&["--armor", "--trust-model", "always", "--pinentry-mode", "loopback", "--passphrase", "", "--recipient", "test@hylki.invalid", "--sign", "--encrypt"], entity);
        let armored = String::from_utf8(enc.stdout.clone()).unwrap();
        assert!(armored.contains("-----BEGIN PGP MESSAGE-----"), "{:?}", enc.status_lines);
        let mail = format!(
            "From: test@hylki.invalid\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n\
             --enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
             --enc\r\nContent-Type: application/octet-stream\r\n\r\n{armored}\r\n--enc--\r\n"
        );
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(u.status.encrypted && u.status.decrypted, "{:?}", u.status);
        let inner = String::from_utf8(u.inner.clone().unwrap()).unwrap();
        assert!(inner.contains("secret hello"), "{inner}");
        assert!(matches!(u.status.signature, PgpSignature::Good { trust: PgpTrust::Full, .. }), "{:?}", u.status);

        // Detached signature over a MIME part, verified the RFC 3156 way.
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nsigned hello";
        let sig = status(&["--armor", "--pinentry-mode", "loopback", "--passphrase", "", "--detach-sign"], part);
        let sig = String::from_utf8(sig.stdout).unwrap();
        let mail = format!(
            "From: test@hylki.invalid\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=\"b\"\r\n\r\n\
             --b\r\n{}\r\n--b\r\nContent-Type: application/pgp-signature\r\n\r\n{sig}\r\n--b--\r\n",
            String::from_utf8_lossy(part)
        );
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(!u.status.encrypted);
        assert!(matches!(u.status.signature, PgpSignature::Good { .. }), "{:?}", u.status);
        assert_eq!(u.inner.unwrap(), part.to_vec());

        // A tampered signed part fails.
        let mail = mail.replace("signed hello", "signed hellO");
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(matches!(u.status.signature, PgpSignature::Bad { .. }), "{:?}", u.status);

        // Encrypted to a key we don't have: reported, nothing rendered.
        let other = "-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----";
        let mail = format!("From: x@y.z\r\nContent-Type: text/plain\r\n\r\n{other}\r\n");
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(u.status.encrypted && !u.status.decrypted);
        assert!(u.inner.is_none());

        // Key management on the same keyring.
        let mine = list_keys(&gpg, true);
        assert_eq!(mine.len(), 1, "{mine:?}");
        assert!(mine[0].can_encrypt && mine[0].secret);
        assert!(secret_key_for(&gpg, "TEST@hylki.invalid").is_some());
        assert!(public_key_for(&gpg, "test@hylki.invalid").is_some());
        assert!(public_key_for(&gpg, "nobody@hylki.invalid").is_none());
        let fpr = generate_key(&gpg, "Second Key", "second@hylki.invalid", "1y", "pw").expect("generated");
        let second = key_by_fingerprint(&gpg, &fpr).expect("listed");
        assert!(second.can_encrypt, "default default gives an encryption subkey: {second:?}");
        assert!(second.expires.is_some());
        let armored = export_public(&gpg, &fpr).expect("exported");
        assert!(String::from_utf8_lossy(&armored).contains("BEGIN PGP PUBLIC KEY BLOCK"));
        // Into a second, empty keyring: imported, untrusted, then vouched for.
        let home2 = std::env::temp_dir().join(format!("hylki-gpg-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home2);
        std::fs::create_dir_all(&home2).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home2, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let gpg2 = Gpg { home: Some(home2.clone()) };
        let s = import_keys(&gpg2, &armored).expect("imported");
        assert_eq!(s.imported, 1);
        assert_eq!(s.fingerprints, vec![fpr.clone()]);
        assert_eq!(import_keys(&gpg2, &armored).unwrap().unchanged, 1);
        assert!(import_keys(&gpg2, b"not a key").is_err());
        assert_eq!(key_by_fingerprint(&gpg2, &fpr).unwrap().validity, KeyValidity::Unknown);
        assert!(trust_key(&gpg2, &fpr, None).is_err(), "no key of one's own to sign with");
        let own = generate_key(&gpg2, "", "me@hylki.invalid", "never", "").expect("own key");
        trust_key(&gpg2, &fpr, Some(&own)).expect("signed");
        assert!(key_by_fingerprint(&gpg2, &fpr).unwrap().validity.trusted());
        // Encrypt to the imported key from the second keyring; the first opens it.
        let ct = encrypt(&gpg2, b"Content-Type: text/plain\r\n\r\nfor second", &["second@hylki.invalid".into()], Some(&own)).expect("encrypted");
        let mail = format!("From: me@hylki.invalid\r\nMIME-Version: 1.0\r\nContent-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"\r\n\r\n--e\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--e\r\nContent-Type: application/octet-stream\r\n\r\n{}\r\n--e--\r\n", String::from_utf8_lossy(&ct));
        // The second key has a passphrase, fed through the loopback for the test.
        let gpg_pw = Gpg { home: Some(home.clone()) };
        let out = run(&gpg_pw, &["--pinentry-mode", "loopback", "--passphrase", "pw", "--decrypt"], &ct, None);
        assert!(String::from_utf8_lossy(&out.stdout).contains("for second"), "{:?}", out.status_lines);
        let _ = mail;
        let (sig, micalg) = sign_detached(&gpg2, b"data", &own).expect("signed");
        assert!(String::from_utf8_lossy(&sig).contains("BEGIN PGP SIGNATURE"));
        assert!(micalg.starts_with("pgp-sha"), "{micalg}");
        assert!(secret_key_by_fingerprint(&gpg2, &own).is_some());
        delete_key(&gpg2, &fpr, false).expect("deleted");
        assert!(key_by_fingerprint(&gpg2, &fpr).is_none());
        delete_key(&gpg2, &own, true).expect("deleted own");
        assert!(list_keys(&gpg2, true).is_empty());

        for h in [&home, &home2] {
            let _ = std::process::Command::new("gpgconf").env("GNUPGHOME", h).args(["--kill", "all"]).status();
            let _ = std::fs::remove_dir_all(h);
        }
    }
}
