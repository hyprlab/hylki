//! Other people's keys that Hylki finds by itself: from Autocrypt headers
//! and gossip in mail that arrives, and by WKD (or, when the user allows it,
//! a keyserver) while a message is being addressed. They go into Hylki's
//! own keyring (`Gpg::discovered`), never the user's, and the cache keeps
//! which key belongs to which address (`PeerState`). What the composer
//! shows and encrypts to is read from both keyrings by [`KeyBook`].

use std::sync::RwLock;

use sha2::{Digest, Sha256};

use super::autocrypt::{self, PeerState, PreferEncrypt, Recommendation};
use super::keys::{import_summary, list_keys};
use super::{run, Gpg, KeyInfo};
use crate::cache::Cache;

/// A WKD miss is asked again after a day, a hit refreshed after a week.
const MISS_TTL: i64 = 24 * 60 * 60;
const HIT_TTL: i64 = 7 * 24 * 60 * 60;
/// The most a key lookup reads; a real key is a few KiB.
const MAX_KEY_BYTES: u64 = 512 * 1024;

/// The digest a keydata blob is remembered by, so the same key arriving
/// in every message is imported once.
pub(crate) fn keydata_digest(keydata: &[u8]) -> String {
    Sha256::digest(keydata)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Import one transferable public key sent for `addr` (an Autocrypt or
/// gossip header) into `gpg` (Hylki's own keyring), returning its
/// fingerprint. Only the user id for `addr` is kept: the header binds the
/// key to that address alone, and a user id naming anyone else would let
/// the key pass for theirs when it signs. Refused when the data holds no
/// key, more than one, or none that names `addr`.
pub(crate) fn import_one(gpg: &Gpg, keydata: &[u8], addr: &str) -> Result<String, String> {
    let filter = format!("keep-uid=mbox = {}", addr.trim().to_ascii_lowercase());
    let out = run(
        gpg,
        &[
            "--import-options",
            "import-clean",
            "--import-filter",
            &filter,
            "--import",
        ],
        keydata,
        None,
    );
    let mut fprs = import_summary(&out).fingerprints;
    fprs.sort();
    fprs.dedup();
    let one = match fprs.as_slice() {
        [one] => one.clone(),
        [] => {
            return Err(out
                .detail
                .unwrap_or_else(|| "no key for the address in the data".into()))
        }
        _ => return Err("more than one key in the data".into()),
    };
    // gpg names a key it already holds as imported even when the filter
    // left nothing of it, so the keyring is asked whether it names `addr`.
    let names_addr = list_keys(gpg, false)
        .iter()
        .any(|k| k.fingerprint == one && k.matches_address(addr));
    if names_addr {
        Ok(one)
    } else {
        Err("the key does not name the address".into())
    }
}

/// Import a key found for `addr` by WKD or a keyserver, keeping only the
/// user ids for that address. `None` when no usable key for encryption to
/// that address came of it: what a server hands out has to name the
/// address it was asked for.
pub(crate) fn import_for_address(
    gpg: &Gpg,
    keydata: &[u8],
    addr: &str,
) -> Result<Option<String>, String> {
    let filter = format!("keep-uid=mbox = {}", addr.to_ascii_lowercase());
    let out = run(
        gpg,
        &[
            "--import-options",
            "import-clean",
            "--import-filter",
            &filter,
            "--import",
        ],
        keydata,
        None,
    );
    let fprs = import_summary(&out).fingerprints;
    let keys = list_keys(gpg, false);
    Ok(fprs.into_iter().find(|f| {
        keys.iter()
            .any(|k| k.fingerprint == *f && encrypts(k) && k.matches_address(addr))
    }))
}

/// A key's public half as an Autocrypt header carries it: binary, with the
/// self-signatures only, the user id for `addr` when it has one, and only
/// the subkeys that encrypt. Under the 10 KiB Level 1 allows, or `None`.
pub(crate) fn export_minimal(gpg: &Gpg, fpr: &str, addr: &str) -> Option<Vec<u8>> {
    let export = |uid_filter: Option<String>| {
        let mut args = vec![
            "--export-options".to_string(),
            "export-minimal".into(),
            "--export-filter".into(),
            "drop-subkey=usage !~ e".into(),
        ];
        if let Some(f) = uid_filter {
            args.extend(["--export-filter".into(), f]);
        }
        args.extend(["--export".into(), fpr.to_string()]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(gpg, &args, b"", None).stdout
    };
    let mut key = export(Some(format!(
        "keep-uid=mbox = {}",
        addr.to_ascii_lowercase()
    )));
    // A key whose user ids name another address still goes whole: the user
    // id is decorative to Autocrypt, which binds the key by `addr=`.
    if key.is_empty() {
        key = export(None);
    }
    (!key.is_empty() && key.len() < 7 * 1024).then_some(key)
}

/// A key's public half from whichever keyring has it, for gossip or for
/// `--recipient-file`.
pub(crate) fn export_key(gpg: &Gpg, fpr: &str) -> Option<Vec<u8>> {
    let out = run(gpg, &["--export", fpr], b"", None);
    (!out.stdout.is_empty()).then_some(out.stdout)
}

fn encrypts(k: &KeyInfo) -> bool {
    k.usable() && k.can_encrypt
}

// ---------------------------------------------------------------------------
// Incoming mail
// ---------------------------------------------------------------------------

/// The accounts Autocrypt is on for, as the reader needs them: their own
/// addresses, which are never anyone's peer. Empty while no account has it
/// on, and then nothing is read.
static AUTOCRYPT_ACCOUNTS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Note which accounts have Autocrypt on, each time the accounts are read
/// or saved. Every address of every account counts as the user's own.
pub(crate) fn note_accounts(accounts: &[crate::config::AccountConfig]) {
    let on = accounts.iter().any(|a| a.enabled && a.autocrypt);
    let own: Vec<String> = if on {
        accounts
            .iter()
            .flat_map(|a| {
                std::iter::once(a.email.to_ascii_lowercase())
                    .chain(a.aliases.iter().map(|al| al.address().to_ascii_lowercase()))
            })
            .collect()
    } else {
        Vec::new()
    };
    if let Ok(mut held) = AUTOCRYPT_ACCOUNTS.write() {
        *held = own;
    }
}

/// Learn from a message as it is read: the sender's Autocrypt header,
/// and, in a decrypted `inner` entity, gossip about the other recipients.
/// Runs on whatever worker thread rendered the message, with a cache
/// connection of that thread's own.
pub(crate) fn observe(raw: &[u8], inner: Option<&[u8]>) {
    let own = match AUTOCRYPT_ACCOUNTS.read() {
        Ok(own) if !own.is_empty() => own.clone(),
        _ => return,
    };
    thread_local! {
        static CACHE: Option<Cache> = Cache::open().ok();
    }
    let Some(gpg) = Gpg::discovered() else {
        return;
    };
    CACHE.with(|cache| {
        if let Some(cache) = cache {
            observe_with(cache, &gpg, raw, inner, &own, crate::datefmt::now());
        }
    });
}

/// [`observe`] with everything it uses handed in.
pub(crate) fn observe_with(
    cache: &Cache,
    gpg: &Gpg,
    raw: &[u8],
    inner: Option<&[u8]>,
    own: &[String],
    now: i64,
) {
    let Some(msg) = autocrypt::incoming(raw, now) else {
        return;
    };
    if own.contains(&msg.from) {
        return;
    }
    let mut peer = cache
        .pgp_peer(&msg.from)
        .unwrap_or_else(|| PeerState::new(&msg.from));
    if !peer.accepts(msg.date) {
        return;
    }
    match &msg.header {
        Some(h) => {
            let digest = keydata_digest(&h.keydata);
            let fpr = if digest == peer.autocrypt_digest && !peer.autocrypt_key.is_empty() {
                Ok(peer.autocrypt_key.clone())
            } else {
                import_one(gpg, &h.keydata, &msg.from)
            };
            match fpr {
                Ok(fpr) => peer.take_autocrypt(msg.date, &fpr, &digest, h.prefer_encrypt),
                Err(e) => {
                    tracing::info!(target: "hylki::pgp", "Autocrypt key of {} not taken: {e}", msg.from);
                    peer.seen(msg.date);
                }
            }
        }
        None => peer.seen(msg.date),
    }
    cache.save_pgp_peer(&peer);

    let Some(inner) = inner else {
        return;
    };
    for g in autocrypt::gossip(inner, &msg.recipients) {
        if own.contains(&g.addr) || g.addr == msg.from {
            continue;
        }
        let mut about = cache
            .pgp_peer(&g.addr)
            .unwrap_or_else(|| PeerState::new(&g.addr));
        if !about.accepts_gossip(msg.date) {
            continue;
        }
        let digest = keydata_digest(&g.keydata);
        let fpr = if digest == about.gossip_digest && !about.gossip_key.is_empty() {
            Ok(about.gossip_key.clone())
        } else {
            import_one(gpg, &g.keydata, &g.addr)
        };
        match fpr {
            Ok(fpr) => {
                about.take_gossip(msg.date, &fpr, &digest);
                cache.save_pgp_peer(&about);
            }
            Err(e) => {
                tracing::info!(target: "hylki::pgp", "gossip about {} not taken: {e}", g.addr)
            }
        }
    }
}

/// The reader's "Fetch the sender's key" for a message whose Autocrypt
/// header is valid for its From (see `inbound::autocrypt_key`): the key
/// goes into Hylki's own keyring, never the user's, where the reader checks
/// signatures against it too. When reading mail has not already recorded a
/// key for the sender, this one is recorded, no newer than the last mail
/// from them. The fingerprint, or why not.
pub(crate) fn take_sender_key(addr: &str, keydata: &[u8]) -> Result<String, String> {
    let gpg = Gpg::discovered().ok_or("no data directory")?;
    let fpr = import_one(&gpg, keydata, addr)?;
    if let Ok(cache) = Cache::open() {
        let mut peer = cache.pgp_peer(addr).unwrap_or_else(|| PeerState::new(addr));
        if peer.autocrypt_key.is_empty() {
            let date = peer.last_seen;
            peer.take_autocrypt(
                date,
                &fpr,
                &keydata_digest(keydata),
                PreferEncrypt::NoPreference,
            );
            cache.save_pgp_peer(&peer);
        }
    }
    Ok(fpr)
}

// ---------------------------------------------------------------------------
// WKD and keyservers
// ---------------------------------------------------------------------------

/// An HTTPS GET: the body, `None` for a 404, or why it failed. A trait so
/// tests can answer without a network.
pub(crate) trait Fetch {
    fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String>;
}

/// The real thing: HTTPS only (a redirect to plain HTTP is refused, as a
/// key fetched in the clear could be anyone's), ten seconds at most.
pub(crate) struct Https;

impl Fetch for Https {
    fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read;
        static AGENT: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| {
            ureq::AgentBuilder::new()
                .https_only(true)
                .timeout(std::time::Duration::from_secs(10))
                .build()
        });
        match AGENT.get(url).call() {
            Ok(resp) => {
                let mut body = Vec::new();
                resp.into_reader()
                    .take(MAX_KEY_BYTES)
                    .read_to_end(&mut body)
                    .map_err(|e| e.to_string())?;
                Ok(Some(body))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// z-base-32, the encoding WKD names its files with.
fn zbase32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for b in bytes {
        buffer = (buffer << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Percent-encoding for a URL query value.
fn query_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The two places WKD (draft-koch-openpgp-webkey-service) keeps the key
/// for an address: the advanced method on `openpgpkey.<domain>`, then the
/// direct one on the domain itself.
pub(crate) fn wkd_urls(addr: &str) -> Option<(String, String)> {
    use sha1::{Digest as _, Sha1};
    let (local, domain) = addr.trim().rsplit_once('@')?;
    if local.is_empty() || domain.is_empty() || domain.contains(['/', '?', '#', ':']) {
        return None;
    }
    let domain = domain.to_ascii_lowercase();
    let hash = zbase32(&Sha1::digest(local.to_ascii_lowercase().as_bytes()));
    let l = query_escape(local);
    Some((
        format!("https://openpgpkey.{domain}/.well-known/openpgpkey/{domain}/hu/{hash}?l={l}"),
        format!("https://{domain}/.well-known/openpgpkey/hu/{hash}?l={l}"),
    ))
}

/// The key WKD publishes for `addr`, if any. The direct method is asked
/// only when the advanced host cannot be reached at all: one that answers
/// "not here" has the say for its domain.
pub(crate) fn wkd_lookup(fetch: &dyn Fetch, addr: &str) -> Result<Option<Vec<u8>>, String> {
    let (advanced, direct) = wkd_urls(addr).ok_or("not an address")?;
    match fetch.get(&advanced) {
        Ok(found) => Ok(found.filter(|k| !k.is_empty())),
        Err(_) => Ok(fetch.get(&direct)?.filter(|k| !k.is_empty())),
    }
}

/// The key keys.openpgp.org has for `addr`, which it hands out only once
/// the owner has confirmed the address.
pub(crate) fn keyserver_lookup(fetch: &dyn Fetch, addr: &str) -> Result<Option<Vec<u8>>, String> {
    let url = format!(
        "https://keys.openpgp.org/vks/v1/by-email/{}",
        query_escape(addr.trim())
    );
    Ok(fetch.get(&url)?.filter(|k| !k.is_empty()))
}

/// Look `addr` up (WKD, then a keyserver when `keyserver` is allowed) and
/// remember the outcome in its state, the key in Hylki's own keyring. The
/// peer as it now stands.
pub(crate) fn discover(
    fetch: &dyn Fetch,
    gpg: &Gpg,
    cache: &Cache,
    addr: &str,
    keyserver: bool,
    now: i64,
) -> PeerState {
    let addr = addr.trim().to_ascii_lowercase();
    let mut peer = cache
        .pgp_peer(&addr)
        .unwrap_or_else(|| PeerState::new(&addr));
    let mut found = None;
    let mut by = "wkd";
    match wkd_lookup(fetch, &addr) {
        Ok(Some(key)) => found = import_for_address(gpg, &key, &addr).ok().flatten(),
        Ok(None) => {}
        Err(e) => tracing::debug!(target: "hylki::pgp", "WKD for {addr}: {e}"),
    }
    if found.is_none() && keyserver {
        by = "keyserver";
        match keyserver_lookup(fetch, &addr) {
            Ok(Some(key)) => found = import_for_address(gpg, &key, &addr).ok().flatten(),
            Ok(None) => {}
            Err(e) => tracing::debug!(target: "hylki::pgp", "keyserver for {addr}: {e}"),
        }
    }
    peer.found_key = found.unwrap_or_default();
    peer.found_by = if peer.found_key.is_empty() {
        String::new()
    } else {
        by.to_string()
    };
    peer.found_checked = now;
    cache.save_pgp_peer(&peer);
    peer
}

/// Whether `peer` is due another lookup.
pub(crate) fn lookup_due(peer: Option<&PeerState>, now: i64) -> bool {
    match peer {
        None => true,
        Some(p) if p.found_checked == 0 => true,
        Some(p) => {
            let ttl = if p.found_key.is_empty() {
                MISS_TTL
            } else {
                HIT_TTL
            };
            now - p.found_checked > ttl
        }
    }
}

// ---------------------------------------------------------------------------
// What the composer shows and encrypts to
// ---------------------------------------------------------------------------

/// Where a recipient's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeySource {
    /// The user's own keyring: imported or vouched for by the user.
    Keyring,
    Autocrypt,
    Gossip,
    Wkd,
    Keyserver,
}

/// One recipient's key, as the composer shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyStatus {
    /// Lowercased.
    pub(crate) addr: String,
    pub(crate) source: Option<KeySource>,
    /// The key in Hylki's own keyring to encrypt to; `None` for a key in
    /// the user's keyring, which gpg finds by address.
    pub(crate) fingerprint: Option<String>,
    pub(crate) recommendation: Recommendation,
}

/// Both keyrings, listed once for a whole set of recipients.
pub(crate) struct KeyBook {
    main: Vec<KeyInfo>,
    discovered: Vec<KeyInfo>,
}

impl KeyBook {
    pub(crate) fn load(main: &Gpg, discovered: Option<&Gpg>) -> KeyBook {
        KeyBook {
            main: list_keys(main, false),
            discovered: discovered.map(|g| list_keys(g, false)).unwrap_or_default(),
        }
    }

    fn usable_found(&self, fpr: &str) -> bool {
        !fpr.is_empty()
            && self
                .discovered
                .iter()
                .any(|k| k.fingerprint.eq_ignore_ascii_case(fpr) && encrypts(k))
    }

    /// One recipient's key and recommendation. Level 1's algorithm decides
    /// for Autocrypt and gossip keys; a key in the user's keyring, or one
    /// published by the address's own domain, makes encryption at least
    /// available (Level 1 allows other sources "at its own discretion"),
    /// and the user's keyring wins over everything Hylki found.
    pub(crate) fn status(
        &self,
        addr: &str,
        peer: Option<&PeerState>,
        own: PreferEncrypt,
        reply_to_encrypted: bool,
    ) -> KeyStatus {
        let addr = addr.trim().to_ascii_lowercase();
        let keyring = self
            .main
            .iter()
            .any(|k| encrypts(k) && k.matches_address(&addr));
        let (autocrypt, target) =
            autocrypt::recommend(peer, |f| self.usable_found(f), own, reply_to_encrypted);
        let found = peer
            .filter(|p| self.usable_found(&p.found_key))
            .map(|p| (p.found_key.clone(), p.found_by.as_str() == "keyserver"));
        let from_header = target
            .as_deref()
            .is_some_and(|t| peer.is_some_and(|p| p.autocrypt_key.eq_ignore_ascii_case(t)));
        let (source, fingerprint) = if keyring {
            (Some(KeySource::Keyring), None)
        } else if from_header && autocrypt >= Recommendation::Available {
            (Some(KeySource::Autocrypt), target.clone())
        } else if let Some((fpr, keyserver)) = found {
            let source = if keyserver {
                KeySource::Keyserver
            } else {
                KeySource::Wkd
            };
            (Some(source), Some(fpr))
        } else if let Some(t) = target.clone() {
            let source = if from_header {
                KeySource::Autocrypt
            } else {
                KeySource::Gossip
            };
            (Some(source), Some(t))
        } else {
            (None, None)
        };
        let recommendation = match source {
            None => Recommendation::Disable,
            Some(KeySource::Autocrypt | KeySource::Gossip) => autocrypt,
            Some(_) => autocrypt.max(Recommendation::Available),
        };
        KeyStatus {
            addr,
            source,
            fingerprint,
            recommendation,
        }
    }
}

/// Every recipient's key status and the message's recommendation, read
/// from both keyrings and the cache: what the composer's badges show.
pub(crate) fn statuses(
    addrs: &[String],
    own: PreferEncrypt,
    reply_to_encrypted: bool,
) -> Vec<KeyStatus> {
    let cache = Cache::open().ok();
    statuses_with(
        &Gpg::system(),
        cache.as_ref(),
        addrs,
        own,
        reply_to_encrypted,
    )
}

/// [`statuses`] read from `main`, the keyring of found keys beside it, and
/// what `cache` keeps about each address.
pub(crate) fn statuses_with(
    main: &Gpg,
    cache: Option<&Cache>,
    addrs: &[String],
    own: PreferEncrypt,
    reply_to_encrypted: bool,
) -> Vec<KeyStatus> {
    let found = main.found_keyring_if_any();
    let book = KeyBook::load(main, found.as_ref());
    addrs
        .iter()
        .map(|a| {
            let peer = cache.and_then(|c| c.pgp_peer(a));
            book.status(a, peer.as_ref(), own, reply_to_encrypted)
        })
        .collect()
}

/// The recommendation for the whole message.
pub(crate) fn overall(statuses: &[KeyStatus]) -> Recommendation {
    let each: Vec<Recommendation> = statuses.iter().map(|s| s.recommendation).collect();
    autocrypt::combine(&each)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wkd_urls_follow_the_draft() {
        // The example in draft-koch-openpgp-webkey-service.
        let (advanced, direct) = wkd_urls("Joe.Doe@Example.ORG").unwrap();
        assert_eq!(
            advanced,
            "https://openpgpkey.example.org/.well-known/openpgpkey/example.org/hu/iy9q119eutrkn8s1mk4r39qejnbu3n5q?l=Joe.Doe"
        );
        assert_eq!(
            direct,
            "https://example.org/.well-known/openpgpkey/hu/iy9q119eutrkn8s1mk4r39qejnbu3n5q?l=Joe.Doe"
        );
        assert!(wkd_urls("nobody").is_none());
        assert!(wkd_urls("a@evil.example/x").is_none());
    }

    /// What a fetch answers.
    type Answer = Result<Option<Vec<u8>>, String>;

    /// A fake network: each URL's answer, and a record of what was asked.
    struct Fake {
        answers: Vec<(String, Answer)>,
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl Fetch for Fake {
        fn get(&self, url: &str) -> Answer {
            self.asked.borrow_mut().push(url.to_string());
            self.answers
                .iter()
                .find(|(u, _)| url.starts_with(u.as_str()))
                .map(|(_, a)| a.clone())
                .unwrap_or(Ok(None))
        }
    }

    fn fake(answers: Vec<(&str, Answer)>) -> Fake {
        Fake {
            answers: answers
                .into_iter()
                .map(|(u, a)| (u.to_string(), a))
                .collect(),
            asked: Default::default(),
        }
    }

    #[test]
    fn wkd_lookup_falls_back_only_when_the_advanced_host_is_unreachable() {
        let key = b"KEY".to_vec();
        let advanced = "https://openpgpkey.example.org/";
        let direct = "https://example.org/";
        let f = fake(vec![(advanced, Ok(Some(key.clone())))]);
        assert_eq!(wkd_lookup(&f, "a@example.org").unwrap(), Some(key.clone()));
        assert_eq!(f.asked.borrow().len(), 1);

        let f = fake(vec![(advanced, Ok(None)), (direct, Ok(Some(key.clone())))]);
        assert_eq!(wkd_lookup(&f, "a@example.org").unwrap(), None);
        assert_eq!(
            f.asked.borrow().len(),
            1,
            "a 404 on the advanced host is final"
        );

        let f = fake(vec![
            (advanced, Err("no such host".into())),
            (direct, Ok(Some(key.clone()))),
        ]);
        assert_eq!(wkd_lookup(&f, "a@example.org").unwrap(), Some(key));
        assert_eq!(f.asked.borrow().len(), 2);

        let f = fake(vec![]);
        assert_eq!(keyserver_lookup(&f, "a+b@example.org").unwrap(), None);
        assert_eq!(
            f.asked.borrow()[0],
            "https://keys.openpgp.org/vks/v1/by-email/a%2Bb%40example.org"
        );
    }

    #[test]
    fn lookups_are_cached_for_a_while() {
        let now = 1_000_000;
        assert!(lookup_due(None, now));
        let mut p = PeerState::new("a@b.c");
        assert!(lookup_due(Some(&p), now));
        p.found_checked = now - 60;
        assert!(!lookup_due(Some(&p), now));
        p.found_checked = now - MISS_TTL - 1;
        assert!(lookup_due(Some(&p), now));
        p.found_key = "F".into();
        assert!(!lookup_due(Some(&p), now));
    }

    #[test]
    fn digests_tell_keys_apart() {
        assert_eq!(keydata_digest(b"a"), keydata_digest(b"a"));
        assert_ne!(keydata_digest(b"a"), keydata_digest(b"b"));
        assert_eq!(keydata_digest(b"").len(), 64);
    }

    /// A keyring of its own in a fresh temporary home, with a key for
    /// `addr` when one is given.
    fn keyring(name: &str, addr: Option<&str>) -> (Gpg, std::path::PathBuf, String) {
        let home = std::env::temp_dir().join(format!("hylki-found-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let gpg = Gpg::at(home.clone());
        let fpr = addr
            .map(|a| super::super::generate_key(&gpg, "", a, "never", "").expect("a key"))
            .unwrap_or_default();
        (gpg, home, fpr)
    }

    /// Autocrypt and gossip against a real gpg: keys land in the keyring of
    /// found keys and never in the user's, a header for someone else is
    /// refused, and what the composer would show follows. Skipped where gpg
    /// is missing.
    #[test]
    fn found_keys_stay_out_of_the_users_keyring() {
        use super::super::autocrypt::header_value;
        use PreferEncrypt::{Mutual, NoPreference};
        if !super::super::available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let (ada, ada_home, ada_fpr) = keyring("ada", Some("ada@example.org"));
        let (bob, bob_home, bob_fpr) = keyring("bob", Some("bob@example.org"));
        let (main, main_home, _) = keyring("me", None);
        let found = main.found_keyring().expect("found keyring");
        let cache = Cache::in_memory().expect("cache");
        let own = vec!["me@example.net".to_string()];

        let ada_key = export_minimal(&ada, &ada_fpr, "ada@example.org").expect("exported");
        let raw = format!(
            "From: Ada <ada@example.org>\r\nTo: me@example.net, bob@example.org\r\n\
             Date: Thu, 1 Oct 2026 10:00:00 +0000\r\nAutocrypt: {}\r\n\r\nhi\r\n",
            header_value("ada@example.org", Some(Mutual), &ada_key)
        );
        observe_with(&cache, &found, raw.as_bytes(), None, &own, i64::MAX);
        let peer = cache.pgp_peer("ada@example.org").expect("recorded");
        assert_eq!(peer.autocrypt_key, ada_fpr);
        assert_eq!(peer.prefer_encrypt, Mutual);
        assert_eq!(peer.autocrypt_timestamp, 1_790_848_800);
        assert!(
            list_keys(&main, false).is_empty(),
            "never the user's keyring"
        );
        let book = KeyBook::load(&main, Some(&found));
        let s = book.status("ada@example.org", Some(&peer), Mutual, false);
        assert_eq!(s.source, Some(KeySource::Autocrypt));
        assert_eq!(s.recommendation, Recommendation::Encrypt);
        assert_eq!(s.fingerprint.as_deref(), Some(ada_fpr.as_str()));
        let s = book.status("ada@example.org", Some(&peer), NoPreference, false);
        assert_eq!(s.recommendation, Recommendation::Available);

        // Mallory's mail with a header naming Ada changes nothing for Ada.
        let forged = raw
            .replace("From: Ada <ada@example.org>", "From: mallory@example.org")
            .replace("1 Oct", "2 Oct");
        observe_with(&cache, &found, forged.as_bytes(), None, &own, i64::MAX);
        assert_eq!(cache.pgp_peer("ada@example.org"), Some(peer.clone()));
        let mallory = cache.pgp_peer("mallory@example.org").expect("seen");
        assert!(mallory.autocrypt_key.is_empty());
        // Nor does a key whose user ids name Ada pass for Mallory's own.
        let lent = raw
            .replace("From: Ada <ada@example.org>", "From: mallory@example.org")
            .replace(
                &header_value("ada@example.org", Some(Mutual), &ada_key),
                &header_value("mallory@example.org", Some(Mutual), &ada_key),
            )
            .replace("1 Oct", "5 Oct");
        observe_with(&cache, &found, lent.as_bytes(), None, &own, i64::MAX);
        let mallory = cache.pgp_peer("mallory@example.org").expect("seen");
        assert!(mallory.autocrypt_key.is_empty(), "{mallory:?}");
        // Nor is the user anyone's peer.
        let mine = raw.replace("From: Ada <ada@example.org>", "From: me@example.net");
        observe_with(&cache, &found, mine.as_bytes(), None, &own, i64::MAX);
        assert!(cache.pgp_peer("me@example.net").is_none());

        // Gossip about Bob, a recipient, inside a decrypted entity; Eve is
        // no recipient of the message.
        let bob_key = export_minimal(&bob, &bob_fpr, "bob@example.org").expect("exported");
        let inner = format!(
            "Content-Type: text/plain\r\nAutocrypt-Gossip: {}\r\nAutocrypt-Gossip: {}\r\n\r\nhi",
            header_value("bob@example.org", None, &bob_key),
            header_value("eve@example.org", None, &bob_key)
        );
        let later = raw.replace("1 Oct", "3 Oct");
        observe_with(
            &cache,
            &found,
            later.as_bytes(),
            Some(inner.as_bytes()),
            &own,
            i64::MAX,
        );
        let bob_peer = cache.pgp_peer("bob@example.org").expect("gossip kept");
        assert_eq!(bob_peer.gossip_key, bob_fpr);
        assert!(cache.pgp_peer("eve@example.org").is_none());
        let book = KeyBook::load(&main, Some(&found));
        let s = book.status("bob@example.org", Some(&bob_peer), Mutual, false);
        assert_eq!(s.source, Some(KeySource::Gossip));
        assert_eq!(s.recommendation, Recommendation::Discourage);

        // WKD, through a fake network: Bob's domain serves his key; a key
        // served for Carol that does not name her is not taken.
        let (bob_wkd, _) = wkd_urls("bob@example.org").unwrap();
        let (carol_wkd, _) = wkd_urls("carol@example.org").unwrap();
        let net = fake(vec![
            (bob_wkd.as_str(), Ok(Some(bob_key.clone()))),
            (carol_wkd.as_str(), Ok(Some(ada_key.clone()))),
        ]);
        let bob_peer = discover(&net, &found, &cache, "bob@example.org", false, 5);
        assert_eq!(
            (bob_peer.found_key.as_str(), bob_peer.found_by.as_str()),
            (bob_fpr.as_str(), "wkd")
        );
        assert_eq!(bob_peer.found_checked, 5);
        assert_eq!(bob_peer.gossip_key, bob_fpr, "the lookup keeps the rest");
        let carol = discover(&net, &found, &cache, "carol@example.org", false, 5);
        assert!(carol.found_key.is_empty());
        assert!(net
            .asked
            .borrow()
            .iter()
            .all(|u| !u.contains("keys.openpgp.org")));
        let book = KeyBook::load(&main, Some(&found));
        let s = book.status("bob@example.org", Some(&bob_peer), Mutual, false);
        assert_eq!(s.source, Some(KeySource::Wkd));
        assert_eq!(s.recommendation, Recommendation::Available);

        // A key in the user's own keyring wins, by address.
        super::super::import_keys(&main, &ada_key).expect("imported");
        let book = KeyBook::load(&main, Some(&found));
        let s = book.status("ada@example.org", Some(&peer), Mutual, false);
        assert_eq!((s.source, s.fingerprint), (Some(KeySource::Keyring), None));
        assert_eq!(s.recommendation, Recommendation::Encrypt);
        let s = book.status("nobody@example.org", None, Mutual, false);
        assert_eq!(
            (s.source, s.recommendation),
            (None, Recommendation::Disable)
        );

        for home in [
            &ada_home,
            &bob_home,
            &main_home.join("hylki-discovered"),
            &main_home,
        ] {
            super::super::drop_test_keyring(home);
        }
    }
}
