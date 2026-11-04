//! Getting mail for an assistant. Nothing here can decrypt: a body comes from
//! the disk cache, which never holds OpenPGP mail, or is rendered here from
//! the raw message, which is looked at first. The app's other route to a body
//! (`MailRequest::LoadBody`) goes through GnuPG and may raise a passphrase
//! prompt, and its memory holds what the user decrypted, so neither is used.

use std::sync::Arc;

use super::{AppCall, AppMsg, Located, Mail};
use crate::models::preview_is_encrypted;

/// How long a message being downloaded is waited for: a server that is slow
/// to answer still answers in this, one that is not there says so sooner.
const FETCH_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

/// A message's body, as far as an assistant gets it.
pub(in crate::mcp) enum Body {
    Html(String),
    Encrypted,
    /// Why it could not be had.
    Unavailable(String),
}

/// Why a raw message is not handed over.
pub(in crate::mcp) enum NoRaw {
    Encrypted,
    Unavailable(String),
}

impl Mail {
    /// A message's body (HTML).
    pub(super) async fn body(&self, account_id: u32, m: &Located) -> Body {
        if preview_is_encrypted(&m.message.preview) {
            return Body::Encrypted;
        }
        let cached = self.lock().load_body(account_id, &m.path, m.uid);
        if let Some(body) = cached {
            return Body::Html(body);
        }
        match self.raw(account_id, m).await {
            Ok(raw) => {
                match tokio::task::spawn_blocking(move || crate::worker::extract_body(&raw)).await {
                    Ok(body) => Body::Html(body),
                    Err(e) => Body::Unavailable(e.to_string()),
                }
            }
            Err(NoRaw::Encrypted) => Body::Encrypted,
            Err(NoRaw::Unavailable(why)) => Body::Unavailable(why),
        }
    }

    /// A message as the server keeps it, unless it is encrypted, asked of
    /// the account's worker as background work: the reader's loads go first,
    /// and none of this interrupts a sync.
    pub(super) async fn raw(&self, account_id: u32, m: &Located) -> Result<Arc<Vec<u8>>, NoRaw> {
        if preview_is_encrypted(&m.message.preview) {
            return Err(NoRaw::Encrypted);
        }
        let key = (account_id, m.path.clone(), m.uid);
        if let Some(raw) = self.shared.memo.get(&key) {
            return Ok(raw);
        }
        let (done, answer) = tokio::sync::oneshot::channel();
        self.app
            .send(AppMsg::Assistant(AppCall::FetchRaw {
                account_id,
                path: m.path.clone(),
                uid: m.uid,
                done,
            }))
            .map_err(|_| NoRaw::Unavailable("Hylki is closing".into()))?;
        let raw = match tokio::time::timeout(FETCH_WAIT, answer).await {
            Ok(Ok(Ok(raw))) => raw,
            Ok(Ok(Err(why))) => return Err(NoRaw::Unavailable(why)),
            Ok(Err(_)) => return Err(NoRaw::Unavailable("Hylki stopped waiting for it".into())),
            Err(_) => {
                tracing::info!("mcp: no message {} {} in {FETCH_WAIT:?}", m.path, m.uid);
                return Err(NoRaw::Unavailable(
                    "the server did not answer in time".into(),
                ));
            }
        };
        let (raw, encrypted) = tokio::task::spawn_blocking(move || {
            let encrypted = crate::mail::pgp::is_encrypted(&raw);
            (raw, encrypted)
        })
        .await
        .map_err(|e| NoRaw::Unavailable(e.to_string()))?;
        if encrypted {
            return Err(NoRaw::Encrypted);
        }
        let raw = Arc::new(raw);
        self.shared.memo.put(key, raw.clone());
        Ok(raw)
    }
}
