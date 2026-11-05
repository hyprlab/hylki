//! OpenPGP correspondents: Autocrypt peer state and key lookups (see
//! `mail::pgp::discovery`).

use rusqlite::{params, OptionalExtension};

use super::Cache;
use crate::mail::pgp::autocrypt::{PeerState, PreferEncrypt};

impl Cache {
    /// What is kept about `addr`, if anything.
    pub(crate) fn pgp_peer(&self, addr: &str) -> Option<PeerState> {
        let addr = addr.trim().to_ascii_lowercase();
        self.conn
            .query_row(
                "SELECT last_seen, autocrypt_timestamp, autocrypt_key, autocrypt_digest, \
                 prefer_encrypt, gossip_timestamp, gossip_key, gossip_digest, found_key, \
                 found_by, found_checked FROM pgp_peers WHERE addr = ?1",
                params![addr],
                |r| {
                    Ok(PeerState {
                        addr: addr.clone(),
                        last_seen: r.get(0)?,
                        autocrypt_timestamp: r.get(1)?,
                        autocrypt_key: r.get(2)?,
                        autocrypt_digest: r.get(3)?,
                        prefer_encrypt: PreferEncrypt::from_mutual(r.get::<_, i64>(4)? != 0),
                        gossip_timestamp: r.get(5)?,
                        gossip_key: r.get(6)?,
                        gossip_digest: r.get(7)?,
                        found_key: r.get(8)?,
                        found_by: r.get(9)?,
                        found_checked: r.get(10)?,
                    })
                },
            )
            .optional()
            .unwrap_or_else(|e| {
                tracing::warn!("cache pgp_peer for {addr} failed: {e}");
                None
            })
    }

    /// Keep `peer`, in place of what was kept for its address.
    pub(crate) fn save_pgp_peer(&self, peer: &PeerState) {
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO pgp_peers (addr, last_seen, autocrypt_timestamp, \
             autocrypt_key, autocrypt_digest, prefer_encrypt, gossip_timestamp, gossip_key, \
             gossip_digest, found_key, found_by, found_checked) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                peer.addr,
                peer.last_seen,
                peer.autocrypt_timestamp,
                peer.autocrypt_key,
                peer.autocrypt_digest,
                i64::from(peer.prefer_encrypt == PreferEncrypt::Mutual),
                peer.gossip_timestamp,
                peer.gossip_key,
                peer.gossip_digest,
                peer.found_key,
                peer.found_by,
                peer.found_checked,
            ],
        ) {
            tracing::warn!("cache save_pgp_peer for {} failed: {e}", peer.addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_are_kept_by_lowercased_address() {
        let cache = Cache::in_memory().expect("cache");
        assert!(cache.pgp_peer("ada@example.org").is_none());
        let mut p = PeerState::new("Ada@Example.org");
        p.take_autocrypt(100, "FPR", "digest", PreferEncrypt::Mutual);
        p.take_gossip(90, "G", "g");
        p.found_key = "W".into();
        p.found_by = "wkd".into();
        p.found_checked = 5;
        cache.save_pgp_peer(&p);
        assert_eq!(cache.pgp_peer("ADA@example.org"), Some(p.clone()));
        p.seen(200);
        cache.save_pgp_peer(&p);
        assert_eq!(cache.pgp_peer("ada@example.org").unwrap().last_seen, 200);
    }
}
