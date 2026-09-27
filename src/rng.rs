//! Cryptographically secure randomness.
//!
//! Used for the OAuth `state` and PKCE verifier, and for the reader's
//! per-render CSP nonce. Every one of those is a security control, so this
//! module fails loudly rather than returning a degenerate value: a caller that
//! silently used an all-zero buffer would produce a *constant* token, which is
//! worse than no token at all.

/// Fill `buf` with random bytes from the OS entropy source.
pub fn fill(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    getrandom::getrandom(buf)
}

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// base64url. A CSP nonce is a `base64-value`, so `.` and `~` — which the
/// URL-unreserved set above contains — are not allowed in one: a nonce carrying
/// either is rejected by the CSP parser and the script it guards silently does
/// not run.
const BASE64URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// A random URL-safe token of `len` characters from the unreserved set.
///
/// Used for the OAuth `state` and the PKCE verifier, whose alphabet RFC 7636
/// defines as exactly this set.
pub fn token(len: usize) -> Result<String, getrandom::Error> {
    from_alphabet(len, UNRESERVED)
}

/// A random `len`-character CSP nonce (base64url).
pub fn nonce(len: usize) -> Result<String, getrandom::Error> {
    from_alphabet(len, BASE64URL)
}

/// `len` random characters drawn uniformly from `alphabet`.
///
/// Rejection sampling, not `% alphabet.len()` — the modulo is biased whenever
/// 256 is not a multiple of the alphabet size. It makes no practical difference
/// at these lengths; getting it right costs one comparison.
pub fn from_alphabet(len: usize, alphabet: &[u8]) -> Result<String, getrandom::Error> {
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    // The largest multiple of the alphabet size that fits in a byte; bytes at or
    // above it are discarded so every character is equally likely.
    let limit = (256 / alphabet.len() * alphabet.len()) as u16;
    while out.len() < len {
        fill(&mut buf)?;
        for &b in buf.iter() {
            if (b as u16) < limit {
                out.push(alphabet[b as usize % alphabet.len()] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::token;

    #[test]
    fn tokens_are_the_length_asked_for_and_url_safe() {
        for len in [1, 24, 63, 64, 65, 200] {
            let t = token(len).expect("entropy");
            assert_eq!(t.chars().count(), len);
            assert!(
                t.chars().all(|c| super::UNRESERVED.contains(&(c as u8))),
                "{t}"
            );
        }
    }

    #[test]
    fn a_nonce_is_valid_in_a_csp() {
        // A CSP nonce is a `base64-value`. The unreserved set `token` draws from
        // also contains `.` and `~`, and a nonce carrying either is rejected by
        // the CSP parser — which silently stops the script it guards from
        // running, roughly half the time, with nothing logged.
        for _ in 0..200 {
            let n = super::nonce(24).expect("entropy");
            assert_eq!(n.chars().count(), 24);
            assert!(
                n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
                "not a valid CSP nonce: {n}"
            );
        }
    }

    #[test]
    fn tokens_are_not_a_constant() {
        // The failure this guards: the old implementation ignored a read error
        // and returned a buffer of zeroes, i.e. the same string every time, as
        // both the anti-CSRF state and the PKCE verifier.
        let a = token(32).expect("entropy");
        let b = token(32).expect("entropy");
        assert_ne!(a, b);
        assert!(a.chars().collect::<std::collections::HashSet<_>>().len() > 4, "{a}");
    }
}
