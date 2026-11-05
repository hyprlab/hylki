//! Message identifiers as the cache keys them. These values are stored, so
//! none of them may change: the uid hash is pinned, and Message-IDs are kept
//! lowercased without brackets (their original case is noted for the wire).

/// The first Message-ID of a header's worth, as the cache keys it.
pub(super) fn first_msgid(ids: &str) -> Option<String> {
    let id = ids
        .split_whitespace()
        .next()?
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase();
    (!id.is_empty()).then_some(id)
}

/// Normalize a single Message-ID: strip angle brackets/whitespace, lowercase.
/// The spelling it arrived in is noted for the wire
/// ([`crate::cache::note_msgid_case`]).
pub(super) fn normalize_msgid(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let exact = s
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    crate::cache::note_msgid_case(exact);
    exact.to_ascii_lowercase()
}

/// Normalize a whitespace-separated list of Message-IDs into a canonical string.
pub(super) fn normalize_msgids(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.split_whitespace()
        .map(|tok| {
            let exact = tok.trim_start_matches('<').trim_end_matches('>').trim();
            crate::cache::note_msgid_case(exact);
            exact.to_ascii_lowercase()
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Combine two normalized Message-ID lists, keeping the first list's order and
/// dropping duplicates. References already contains In-Reply-To on a well-formed
/// message; on a malformed one either may hold the only usable link.
pub(super) fn merge_msgids(a: &str, b: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for id in a.split_whitespace().chain(b.split_whitespace()) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out.join(" ")
}

/// The synthetic reference tokens the API backends thread by, where a
/// listing carries the server's conversation id but no References header:
/// Microsoft 365 (`graph-conv:`) and Tuta (`tuta-conv:`). They are internal
/// glue for the threader and must never reach a wire header.
pub(super) const GRAPH_CONV: &str = "graph-conv:";
pub(super) const TUTA_CONV: &str = "tuta-conv:";

/// Whether a References token is one of the synthetic ones above.
pub(super) fn is_conversation_token(token: &str) -> bool {
    token.starts_with(GRAPH_CONV) || token.starts_with(TUTA_CONV)
}

/// Stable u32 id derived from a server's string id (POP3 UIDL, JMAP and Graph
/// message ids), since the rest of the app keys messages by u32. The ids are
/// kept in the cache, so the hash must never change: it is pinned to SipHash-1-3
/// with zero keys, which is what std's `DefaultHasher` gave when they were
/// first stored, rather than left to `DefaultHasher`, which may change.
pub(super) fn hash_uid(uid: &str) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = siphasher::sip::SipHasher13::new();
    uid.hash(&mut h);
    (h.finish() & 0x7fff_ffff) as u32
}

#[cfg(test)]
mod hash_uid_tests {
    use super::hash_uid;

    /// The ids already in users' caches, as std's DefaultHasher derived them.
    #[test]
    fn ids_match_the_ones_already_stored() {
        assert_eq!(hash_uid(""), 600129007);
        assert_eq!(hash_uid("0000001a4f2c"), 673950292);
        assert_eq!(hash_uid("M1b2c3d4e5f6@x"), 531181911);
        assert_eq!(hash_uid("AAMkAGI2TG93AAA="), 581220562);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_and_in_reply_to_merge_without_duplicates() {
        // A well-formed reply: In-Reply-To repeats the last id of References, and
        // the ancestry it adds is what lets a thread survive a parent kept in
        // another folder (#21).
        assert_eq!(
            merge_msgids("a@x b@y", "b@y"),
            "a@x b@y",
            "the repeated parent should not be listed twice"
        );
        // Either side may be the only one present.
        assert_eq!(merge_msgids("", "b@y"), "b@y");
        assert_eq!(merge_msgids("a@x", ""), "a@x");
        assert_eq!(merge_msgids("", ""), "");
        // A malformed reply whose In-Reply-To names an ancestor References omits.
        assert_eq!(merge_msgids("a@x", "c@z"), "a@x c@z");
    }

    #[test]
    fn first_msgid_is_lowercased_without_brackets() {
        assert_eq!(first_msgid("<A@X> <b@y>").as_deref(), Some("a@x"));
        assert_eq!(first_msgid("  plain@id  ").as_deref(), Some("plain@id"));
        assert_eq!(first_msgid("<>"), None);
        assert_eq!(first_msgid(""), None);
    }
}
