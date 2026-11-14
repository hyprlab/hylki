//! Rendering a fetched message for the reader: the body HTML, the sender
//! check with any OpenPGP verdict, and the attachment list.

use crate::worker::render::attachments::extract_attachments;
use crate::worker::{extract_body, wrap_plain};

pub(super) mod attachments;
pub(super) mod body;
#[cfg(test)]
mod tests;

/// What a raw message renders to: body HTML, the sender check (carrying the
/// OpenPGP verdict, #133, when the message had any), and whether it has
/// attachments. An encrypted message is decrypted through the user's GnuPG
/// here, which may raise the agent's passphrase prompt — so this runs only
/// for an explicit open, never for the background prefetch.
pub(crate) fn render_raw(raw: &[u8]) -> (String, crate::models::SenderCheck, bool) {
    let mut check = crate::mail::verify::check_sender(raw);
    // Autocrypt (Level 1) takes keys only from mail whose From the
    // receiving server authenticated: anyone can type a From, and a key
    // taken from a forged one would be encrypted to without asking.
    let authentic = check.trust == crate::models::SenderTrust::Pass;
    match crate::mail::pgp::unwrap_message(raw, &crate::mail::pgp::Gpg::system()) {
        Some(mut u) => {
            // Gossip only comes from inside a decrypted message.
            let decrypted = u.inner.as_deref().filter(|_| u.status.decrypted);
            if authentic {
                crate::mail::pgp::discovery::observe(raw, decrypted);
            } else {
                // Nor does the reader offer to import it.
                u.status.autocrypt = None;
            }
            let body = match &u.inner {
                Some(inner) => extract_body(inner),
                None => wrap_plain(&pgp_notice(&u.status)),
            };
            let has = u
                .inner
                .as_deref()
                .is_some_and(|i| !extract_attachments(i).is_empty());
            check.pgp = Some(u.status);
            (body, check, has)
        }
        None => {
            if authentic {
                crate::mail::pgp::discovery::observe(raw, None);
            }
            (
                extract_body(raw),
                check,
                !extract_attachments(raw).is_empty(),
            )
        }
    }
}

/// The reader's stand-in for a message that could not be decrypted.
fn pgp_notice(status: &crate::models::PgpStatus) -> String {
    let mut text = status.summary();
    for n in &status.notes {
        text.push('\n');
        text.push_str(n);
    }
    text
}

/// Whether a rendered body may go into the cache: never for anything
/// OpenPGP. An encrypted message's plaintext lives only in the reader, and
/// a failed decryption is retried at the next open. A signed one is fetched
/// and verified afresh each time too, so importing or trusting the sender's
/// key changes the verdict at the next open — a cached verdict would be the
/// keyring as it stood the first time, for ever.
pub(super) fn body_cacheable(check: &crate::models::SenderCheck) -> bool {
    check.pgp.is_none()
}
