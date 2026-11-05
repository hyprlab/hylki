//! Autocrypt Level 1 (<https://autocrypt.org/level1.html>): the header
//! that carries a sender's key, the state kept for each peer, and the
//! recommendation the composer follows. Everything here is pure; importing
//! keys and the stored state are in `discovery` and the cache.

use crate::encoding::{base64_decode, base64_encode};

/// The largest Autocrypt header Level 1 accepts, whole. A bigger one is
/// most likely a bloated key, and every client failing the same way on it
/// is the point.
const MAX_HEADER: usize = 10 * 1024;

/// An Autocrypt key older than this, against the last mail from the peer,
/// suggests they moved to a client without Autocrypt (Level 1, 35 days).
const STALE_SECS: i64 = 35 * 24 * 60 * 60;

/// A peer's or an account's encryption preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PreferEncrypt {
    #[default]
    NoPreference,
    Mutual,
}

impl PreferEncrypt {
    pub(crate) fn from_mutual(mutual: bool) -> PreferEncrypt {
        if mutual {
            PreferEncrypt::Mutual
        } else {
            PreferEncrypt::NoPreference
        }
    }
}

/// One valid `Autocrypt:` or `Autocrypt-Gossip:` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutocryptHeader {
    /// Lowercased.
    pub(crate) addr: String,
    pub(crate) prefer_encrypt: PreferEncrypt,
    /// The binary transferable public key.
    pub(crate) keydata: Vec<u8>,
}

/// Read one header value. `None` when Level 1 calls it invalid: no `addr`
/// or `keydata`, `keydata` not last, a critical attribute it does not know,
/// or over 10 KiB.
pub(crate) fn parse_header(value: &str) -> Option<AutocryptHeader> {
    if value.len() > MAX_HEADER {
        return None;
    }
    let mut addr = None;
    let mut prefer = PreferEncrypt::NoPreference;
    let mut keydata = None;
    for attr in value.split(';') {
        let attr = attr.trim();
        if attr.is_empty() {
            continue;
        }
        // Anything after keydata makes the header invalid: it must be last.
        if keydata.is_some() {
            return None;
        }
        let (name, val) = attr.split_once('=')?;
        let (name, val) = (name.trim(), val.trim());
        match name {
            "addr" if addr.is_none() => addr = Some(val.to_ascii_lowercase()),
            "prefer-encrypt" => {
                if val == "mutual" {
                    prefer = PreferEncrypt::Mutual;
                }
            }
            "keydata" => keydata = Some(val.split_whitespace().collect::<String>()),
            n if n.starts_with('_') => {}
            _ => return None,
        }
    }
    let addr = addr.filter(|a| is_plain_address(a))?;
    let keydata = base64_decode(&keydata?).filter(|k| !k.is_empty())?;
    Some(AutocryptHeader {
        addr,
        prefer_encrypt: prefer,
        keydata,
    })
}

/// An address with nothing in it that could break out of the header.
fn is_plain_address(addr: &str) -> bool {
    addr.contains('@')
        && !addr
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ';' | '<' | '>' | ','))
}

/// The value of an `Autocrypt:` header for an outgoing message, folded so
/// no line runs past 78 characters. Gossip leaves `prefer` out.
pub(crate) fn header_value(addr: &str, prefer: Option<PreferEncrypt>, keydata: &[u8]) -> String {
    let mut out = format!("addr={addr};");
    if prefer == Some(PreferEncrypt::Mutual) {
        out.push_str(" prefer-encrypt=mutual;");
    }
    out.push_str(" keydata=");
    let b64 = base64_encode(keydata);
    for chunk in b64.as_bytes().chunks(76) {
        out.push_str("\r\n ");
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
    }
    out
}

/// A header block's fields, unfolded, in order. Stops at the blank line.
pub(crate) fn header_fields(raw: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = fields.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        match line.split_once(':') {
            Some((name, value)) => fields.push((name.trim().to_string(), value.trim().to_string())),
            None => break,
        }
    }
    fields
}

/// The values of every field called `name` (any case).
fn fields_named<'a>(fields: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    fields
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect()
}

/// The addresses in a field such as To, lowercased.
fn addresses(value: &str) -> Vec<String> {
    crate::worker::parse_recipients(value)
        .into_iter()
        .map(|(_, a)| a.trim().to_ascii_lowercase())
        .filter(|a| !a.is_empty())
        .collect()
}

/// What an incoming message says for Autocrypt: its sender, its effective
/// date, and the sender's valid header if it carried exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Incoming {
    pub(crate) from: String,
    pub(crate) date: i64,
    pub(crate) header: Option<AutocryptHeader>,
    /// The To, Cc and Reply-To addresses, which gossip may only be about.
    pub(crate) recipients: Vec<String>,
}

/// Read an incoming message's headers for Autocrypt, `now` being the time
/// of receipt. `None` for what Level 1 says to ignore: a report (receipts,
/// bounces), a From with more or fewer than one address, or no Date (an
/// undated message seen again and again would keep moving `last_seen`).
pub(crate) fn incoming(raw: &[u8], now: i64) -> Option<Incoming> {
    let fields = header_fields(raw);
    let content_type = fields_named(&fields, "Content-Type")
        .first()
        .map(|c| c.to_ascii_lowercase())
        .unwrap_or_default();
    if content_type.starts_with("multipart/report") {
        return None;
    }
    let from = single_from(&fields)?;
    let date = mail_parser::MessageParser::default()
        .parse_headers(raw)?
        .date()?
        .to_timestamp();
    // A date from the future counts as now.
    let date = date.min(now);
    let header = header_for(&fields, &from);
    let mut recipients = Vec::new();
    for name in ["To", "Cc", "Reply-To"] {
        for v in fields_named(&fields, name) {
            recipients.extend(addresses(v));
        }
    }
    Some(Incoming {
        from: from.clone(),
        date,
        header,
        recipients,
    })
}

/// The one sender's address, when the From names exactly one.
fn single_from(fields: &[(String, String)]) -> Option<String> {
    let from = fields_named(fields, "From");
    let [from] = from.as_slice() else {
        return None;
    };
    let from = addresses(from);
    let [from] = from.as_slice() else {
        return None;
    };
    Some(from.clone())
}

/// The single valid `Autocrypt:` header for `from`. More than one valid
/// header is an error, and then none of them counts.
fn header_for(fields: &[(String, String)], from: &str) -> Option<AutocryptHeader> {
    let valid: Vec<AutocryptHeader> = fields_named(fields, "Autocrypt")
        .into_iter()
        .filter_map(parse_header)
        .filter(|h| h.addr == from)
        .collect();
    match <[AutocryptHeader; 1]>::try_from(valid) {
        Ok([h]) => Some(h),
        Err(_) => None,
    }
}

/// The sender's Autocrypt header in a raw message, if it has a valid one
/// for its From: what the reader offers to import.
pub(crate) fn sender_header(raw: &[u8]) -> Option<AutocryptHeader> {
    let fields = header_fields(raw);
    header_for(&fields, &single_from(&fields)?)
}

/// The `Autocrypt-Gossip` headers of a decrypted entity that are about one
/// of the message's own recipients; the others are ignored.
pub(crate) fn gossip(entity: &[u8], recipients: &[String]) -> Vec<AutocryptHeader> {
    let fields = header_fields(entity);
    fields_named(&fields, "Autocrypt-Gossip")
        .into_iter()
        .filter_map(parse_header)
        .filter(|h| recipients.contains(&h.addr))
        .collect()
}

/// What Autocrypt keeps about one correspondent (Level 1 "peers"), plus
/// what Hylki found by looking the address up (WKD, or a keyserver). A key
/// is named by its fingerprint in Hylki's own keyring; the digest is of
/// the keydata, so the same key arriving again is not imported again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PeerState {
    pub(crate) addr: String,
    pub(crate) last_seen: i64,
    pub(crate) autocrypt_timestamp: i64,
    pub(crate) autocrypt_key: String,
    pub(crate) autocrypt_digest: String,
    pub(crate) prefer_encrypt: PreferEncrypt,
    pub(crate) gossip_timestamp: i64,
    pub(crate) gossip_key: String,
    pub(crate) gossip_digest: String,
    /// The key a lookup found, and where: "wkd" or "keyserver".
    pub(crate) found_key: String,
    pub(crate) found_by: String,
    /// When the address was last looked up; 0 for never.
    pub(crate) found_checked: i64,
}

impl PeerState {
    pub(crate) fn new(addr: &str) -> PeerState {
        PeerState {
            addr: addr.to_ascii_lowercase(),
            ..Default::default()
        }
    }

    /// Whether a message of this date may update the state at all: one
    /// older than the newest Autocrypt header changes nothing.
    pub(crate) fn accepts(&self, date: i64) -> bool {
        date >= self.autocrypt_timestamp
    }

    /// A message from the peer of this date, header or not.
    pub(crate) fn seen(&mut self, date: i64) {
        if self.accepts(date) && date > self.last_seen {
            self.last_seen = date;
        }
    }

    /// Its valid header, the key it carried imported as `fingerprint`.
    pub(crate) fn take_autocrypt(
        &mut self,
        date: i64,
        fingerprint: &str,
        digest: &str,
        prefer: PreferEncrypt,
    ) {
        if !self.accepts(date) {
            return;
        }
        self.seen(date);
        self.autocrypt_timestamp = date;
        self.autocrypt_key = fingerprint.to_string();
        self.autocrypt_digest = digest.to_string();
        self.prefer_encrypt = prefer;
    }

    /// Whether gossip of this date is newer than what is kept.
    pub(crate) fn accepts_gossip(&self, date: i64) -> bool {
        date >= self.gossip_timestamp
    }

    /// Gossip about the peer in someone else's encrypted message.
    pub(crate) fn take_gossip(&mut self, date: i64, fingerprint: &str, digest: &str) {
        if !self.accepts_gossip(date) {
            return;
        }
        self.gossip_timestamp = date;
        self.gossip_key = fingerprint.to_string();
        self.gossip_digest = digest.to_string();
    }
}

/// The Autocrypt recommendation for the encryption control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Recommendation {
    /// Encryption is not possible.
    Disable,
    /// Possible, but the recipient may not read it any more.
    Discourage,
    /// Possible; the user chooses.
    Available,
    /// Possible, and on by default.
    Encrypt,
}

/// The recommendation for one recipient, and the key it would use:
/// `usable` says whether a fingerprint can still encrypt (not revoked or
/// expired). Level 1, "Recommendations for single-recipient messages".
pub(crate) fn recommend(
    peer: Option<&PeerState>,
    usable: impl Fn(&str) -> bool,
    own: PreferEncrypt,
    reply_to_encrypted: bool,
) -> (Recommendation, Option<String>) {
    let Some(peer) = peer else {
        return (Recommendation::Disable, None);
    };
    let key = |fpr: &str| (!fpr.is_empty() && usable(fpr)).then(|| fpr.to_string());
    let (preliminary, target) = match (key(&peer.autocrypt_key), key(&peer.gossip_key)) {
        (Some(k), _) => {
            let stale = peer.last_seen - peer.autocrypt_timestamp > STALE_SECS;
            let r = if stale {
                Recommendation::Discourage
            } else {
                Recommendation::Available
            };
            (r, k)
        }
        (None, Some(k)) => (Recommendation::Discourage, k),
        (None, None) => return (Recommendation::Disable, None),
    };
    let encrypt = reply_to_encrypted
        || (preliminary == Recommendation::Available
            && peer.prefer_encrypt == PreferEncrypt::Mutual
            && own == PreferEncrypt::Mutual);
    let r = if encrypt {
        Recommendation::Encrypt
    } else {
        preliminary
    };
    (r, Some(target))
}

/// The recommendation for a message to several recipients, from each
/// one's own. No recipients is `Disable`.
pub(crate) fn combine(each: &[Recommendation]) -> Recommendation {
    if each.is_empty() || each.contains(&Recommendation::Disable) {
        Recommendation::Disable
    } else if each.iter().all(|r| *r == Recommendation::Encrypt) {
        Recommendation::Encrypt
    } else if each.contains(&Recommendation::Discourage) {
        Recommendation::Discourage
    } else {
        Recommendation::Available
    }
}

/// `Autocrypt-Draft-State` (Level 1, "Storing Draft State"): an encrypted
/// draft says so, so it is encrypted again when it is sent.
pub(crate) const DRAFT_STATE: &str = "Autocrypt-Draft-State";

/// The value an encrypted draft carries.
pub(crate) fn draft_state(encrypt: bool) -> String {
    format!(
        "encrypt={}; _by-choice=yes;",
        if encrypt { "yes" } else { "no" }
    )
}

/// Whether a draft's `Autocrypt-Draft-State` asks for encryption. A
/// critical attribute this does not know makes the header void.
pub(crate) fn draft_wants_encryption(raw: &[u8]) -> bool {
    let fields = header_fields(raw);
    let Some(value) = fields_named(&fields, DRAFT_STATE).first().copied() else {
        return false;
    };
    let mut encrypt = false;
    for attr in value.split(';').map(str::trim).filter(|a| !a.is_empty()) {
        let Some((name, val)) = attr.split_once('=') else {
            return false;
        };
        match name.trim() {
            "encrypt" => encrypt = val.trim() == "yes",
            n if n.starts_with('_') => {}
            _ => return false,
        }
    }
    encrypt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Vec<u8> {
        (0u8..200).collect()
    }

    #[test]
    fn headers_round_trip_and_fold() {
        let value = header_value("Ada@Example.org", Some(PreferEncrypt::Mutual), &key());
        assert!(value.lines().all(|l| l.len() <= 78), "{value}");
        let parsed = parse_header(&value.replace("\r\n", "")).expect("valid");
        assert_eq!(parsed.addr, "ada@example.org");
        assert_eq!(parsed.prefer_encrypt, PreferEncrypt::Mutual);
        assert_eq!(parsed.keydata, key());
        let gossip = header_value("bob@example.org", None, &key());
        assert!(!gossip.contains("prefer-encrypt"));
    }

    #[test]
    fn invalid_headers_are_refused() {
        let kd = base64_encode(&key());
        assert!(parse_header(&format!("keydata={kd}")).is_none(), "no addr");
        assert!(parse_header("addr=a@b.c").is_none(), "no keydata");
        assert!(parse_header(&format!("addr=a@b.c; keydata={kd}; x=1")).is_none());
        assert!(parse_header(&format!("addr=a@b.c; critical=1; keydata={kd}")).is_none());
        assert!(parse_header(&format!("addr=a b@c; keydata={kd}")).is_none());
        assert!(parse_header(&format!("addr=a@b.c; keydata={}", "A".repeat(11000))).is_none());
        let ok = parse_header(&format!(
            "addr=a@b.c; _extra=1; prefer-encrypt=whatever; keydata={kd}"
        ))
        .expect("non-critical attributes are ignored");
        assert_eq!(ok.prefer_encrypt, PreferEncrypt::NoPreference);
    }

    fn mail(headers: &str) -> Vec<u8> {
        format!("{headers}\r\n\r\nbody\r\n").into_bytes()
    }

    #[test]
    fn incoming_checks_addr_against_from() {
        let kd = base64_encode(&key());
        let raw = mail(&format!(
            "From: Ada <ada@example.org>\r\nTo: me@example.net\r\nDate: Thu, 1 Oct 2026 10:00:00 +0000\r\n\
             Autocrypt: addr=ada@example.org; prefer-encrypt=mutual;\r\n keydata={kd}"
        ));
        let inc = incoming(&raw, i64::MAX).expect("read");
        assert_eq!(inc.from, "ada@example.org");
        assert_eq!(inc.date, 1_790_848_800);
        assert_eq!(inc.recipients, vec!["me@example.net".to_string()]);
        assert_eq!(inc.header.unwrap().prefer_encrypt, PreferEncrypt::Mutual);

        // A header for someone else is not the sender's.
        let forged = mail(&format!(
            "From: mallory@example.org\r\nDate: Thu, 1 Oct 2026 10:00:00 +0000\r\n\
             Autocrypt: addr=ada@example.org; keydata={kd}"
        ));
        assert!(incoming(&forged, i64::MAX).unwrap().header.is_none());

        // Two valid headers cancel each other.
        let twice = mail(&format!(
            "From: ada@example.org\r\nDate: Thu, 1 Oct 2026 10:00:00 +0000\r\n\
             Autocrypt: addr=ada@example.org; keydata={kd}\r\nAutocrypt: addr=ada@example.org; keydata={kd}"
        ));
        assert!(incoming(&twice, i64::MAX).unwrap().header.is_none());

        // Reports, several senders and undated mail are skipped; a future
        // date counts as now.
        let report = mail("From: a@b.c\r\nDate: Thu, 1 Oct 2026 10:00:00 +0000\r\nContent-Type: multipart/report; report-type=disposition-notification");
        assert!(incoming(&report, i64::MAX).is_none());
        assert!(incoming(
            &mail("From: a@b.c, d@e.f\r\nDate: Thu, 1 Oct 2026 10:00:00 +0000"),
            i64::MAX
        )
        .is_none());
        assert!(incoming(&mail("From: a@b.c"), i64::MAX).is_none());
        assert_eq!(
            incoming(
                &mail("From: a@b.c\r\nDate: Thu, 1 Oct 2026 10:00:00 +0000"),
                5
            )
            .unwrap()
            .date,
            5
        );
    }

    #[test]
    fn peer_state_follows_level_one() {
        let mut p = PeerState::new("Ada@Example.org");
        assert_eq!(p.addr, "ada@example.org");
        p.take_autocrypt(100, "FPR1", "d1", PreferEncrypt::Mutual);
        assert_eq!(
            (p.last_seen, p.autocrypt_timestamp, p.autocrypt_key.as_str()),
            (100, 100, "FPR1")
        );
        // Mail without a header only moves last_seen.
        p.seen(200);
        assert_eq!((p.last_seen, p.autocrypt_timestamp), (200, 100));
        // An older header changes nothing.
        p.take_autocrypt(50, "OLD", "d0", PreferEncrypt::NoPreference);
        assert_eq!(p.autocrypt_key, "FPR1");
        // Nor does older mail move last_seen back, or forward past a newer header.
        p.seen(150);
        assert_eq!(p.last_seen, 200);
        p.take_autocrypt(300, "FPR2", "d2", PreferEncrypt::NoPreference);
        assert_eq!(
            (p.last_seen, p.autocrypt_key.as_str(), p.prefer_encrypt),
            (300, "FPR2", PreferEncrypt::NoPreference)
        );
        // Gossip keeps the newest.
        p.take_gossip(400, "G1", "g1");
        p.take_gossip(350, "G0", "g0");
        assert_eq!((p.gossip_timestamp, p.gossip_key.as_str()), (400, "G1"));
    }

    #[test]
    fn recommendation_table() {
        use PreferEncrypt::*;
        use Recommendation::*;
        let all = |_: &str| true;
        let day = 24 * 60 * 60;
        let peer = |ts: i64, seen: i64, key: &str, gossip: &str, prefer: PreferEncrypt| PeerState {
            addr: "p@x".into(),
            last_seen: seen,
            autocrypt_timestamp: ts,
            autocrypt_key: key.into(),
            prefer_encrypt: prefer,
            gossip_key: gossip.into(),
            ..Default::default()
        };
        // (peer, own preference, reply to encrypted) -> recommendation
        let cases: Vec<(Option<PeerState>, PreferEncrypt, bool, Recommendation)> = vec![
            (None, Mutual, false, Disable),
            (Some(peer(0, 0, "", "", Mutual)), Mutual, false, Disable),
            (Some(peer(0, 0, "", "", Mutual)), Mutual, true, Disable),
            (
                Some(peer(10, 10, "K", "", NoPreference)),
                Mutual,
                false,
                Available,
            ),
            (
                Some(peer(10, 10, "K", "", Mutual)),
                NoPreference,
                false,
                Available,
            ),
            (Some(peer(10, 10, "K", "", Mutual)), Mutual, false, Encrypt),
            (
                Some(peer(0, 36 * day, "K", "", Mutual)),
                Mutual,
                false,
                Discourage,
            ),
            (
                Some(peer(0, 34 * day, "K", "", Mutual)),
                Mutual,
                false,
                Encrypt,
            ),
            (
                Some(peer(0, 36 * day, "K", "", Mutual)),
                Mutual,
                true,
                Encrypt,
            ),
            (Some(peer(0, 0, "", "G", Mutual)), Mutual, false, Discourage),
            (
                Some(peer(0, 0, "", "G", Mutual)),
                NoPreference,
                true,
                Encrypt,
            ),
        ];
        for (i, (p, own, reply, want)) in cases.into_iter().enumerate() {
            assert_eq!(recommend(p.as_ref(), all, own, reply).0, want, "case {i}");
        }
        // An unusable key counts as none: the gossip key stands in.
        let p = peer(10, 10, "REVOKED", "G", Mutual);
        assert_eq!(
            recommend(Some(&p), |f| f != "REVOKED", Mutual, false),
            (Discourage, Some("G".into()))
        );

        assert_eq!(combine(&[]), Disable);
        assert_eq!(combine(&[Encrypt, Disable]), Disable);
        assert_eq!(combine(&[Encrypt, Encrypt]), Encrypt);
        assert_eq!(combine(&[Encrypt, Discourage, Available]), Discourage);
        assert_eq!(combine(&[Encrypt, Available]), Available);
    }

    #[test]
    fn gossip_is_only_about_the_recipients() {
        let g = |a: &str| format!("Autocrypt-Gossip: {}", header_value(a, None, &key()));
        let entity = format!(
            "Content-Type: text/plain\r\n{}\r\n{}\r\n\r\nhi",
            g("bob@example.org"),
            g("eve@example.org")
        );
        let found = gossip(entity.as_bytes(), &["bob@example.org".into()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].addr, "bob@example.org");
        assert_eq!(found[0].keydata, key());
    }

    #[test]
    fn draft_state_reads_back() {
        let raw = format!("{DRAFT_STATE}: {}\r\n\r\n", draft_state(true));
        assert!(draft_wants_encryption(raw.as_bytes()));
        assert!(!draft_wants_encryption(
            format!("{DRAFT_STATE}: {}\r\n\r\n", draft_state(false)).as_bytes()
        ));
        assert!(!draft_wants_encryption(
            format!("{DRAFT_STATE}: encrypt=yes; future=1\r\n\r\n").as_bytes()
        ));
        assert!(!draft_wants_encryption(b"Subject: x\r\n\r\n"));
    }
}
