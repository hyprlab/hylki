//! Reads that survive a dropped connection: take the session, try once,
//! reconnect and try again, and put the session back.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::Message;
use crate::worker::imap::bodies::{load_bodies, load_body, load_raw, load_raws, load_source};
use crate::worker::imap::connect::connect;
use crate::worker::imap::fetch_mode::{is_parse_error, previews_rejected, step_fetch_mode};
use crate::worker::imap::sync::load_messages;
use crate::worker::imap::ImapSession;

/// Run `load_messages`, retrying once over a fresh login if the first attempt
/// fails (typically a server-dropped idle connection: "unexpected EOF"). On
/// success the (possibly new) session is stored back; on failure it is dropped.
pub(super) async fn load_messages_retry(
    account_id: u32,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    folder_id: u32,
    path: &str,
    use_envelope: &mut bool,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut result = load_messages(account_id, &mut s, folder_id, path, *use_envelope, cache).await;

    // A non-empty success is trustworthy — keep the session and return it.
    if matches!(&result, Ok(msgs) if !msgs.is_empty()) {
        *session = Some(s);
        return result;
    }

    // Otherwise the result is an error (stale connection → EOF, or a reply
    // our parser rejected) or an empty mailbox (which a stale session can
    // return without erroring). Re-verify on a fresh login. An *unverified*
    // empty result is treated as a failure so it can never wipe cached mail.
    //
    // A reply the parser rejected means the server answers this way of
    // asking with something non-compliant — iCloud's ENVELOPE, Mailfence's
    // FETCH once the preview items are in it (#226) — so each such failure
    // moves the account one step down the [`step_fetch_mode`] ladder for the
    // rest of the session, until a way works or the ladder runs out. Any
    // other error is retried once as it was.
    let mut stepped = false;
    for _ in 0..4 {
        if let Err(e) = &result {
            if is_parse_error(e) && !step_fetch_mode(account_id, use_envelope) {
                return result;
            }
            stepped |= is_parse_error(e);
        }
        match connect(account).await {
            Ok(fresh) => s = fresh,
            Err(_) => {
                return match result {
                    Ok(_) => Err(async_imap::error::Error::ConnectionLost),
                    Err(e) => Err(e),
                }
            }
        }
        result = load_messages(account_id, &mut s, folder_id, path, *use_envelope, cache).await;
        match &result {
            Ok(_) => {
                *session = Some(s);
                // A mode reached by stepping has now loaded a folder: the
                // next launch starts here.
                if stepped {
                    crate::config::save_fetch_mode(
                        &account.email,
                        *use_envelope,
                        previews_rejected(account_id),
                    );
                }
                return result;
            }
            Err(e) if is_parse_error(e) => continue,
            Err(_) => return result,
        }
    }
    result
}

/// Run a read on the session, and once more on a fresh login if it fails
/// (typically a server-dropped idle connection). On success the (possibly
/// new) session is stored back; on failure it is dropped.
async fn with_one_retry<T>(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    mut read: impl AsyncFnMut(&mut ImapSession) -> Result<T, async_imap::error::Error>,
) -> Result<T, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = read(&mut s).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = read(&mut s).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// Like [`load_messages_retry`], but for a single body.
pub(super) async fn load_body_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<(String, crate::models::SenderCheck, bool), async_imap::error::Error> {
    with_one_retry(session, account, async |s| load_body(s, path, uid).await).await
}

/// [`load_raws`] with [`load_body_retry`]'s one reconnect-and-retry.
pub(super) async fn load_raws_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uids: &[u32],
) -> Result<std::collections::HashMap<u32, Vec<u8>>, async_imap::error::Error> {
    with_one_retry(session, account, async |s| load_raws(s, path, uids).await).await
}

/// [`load_bodies`] with [`load_body_retry`]'s one reconnect-and-retry.
pub(super) async fn load_bodies_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uids: &[u32],
) -> Result<
    std::collections::HashMap<u32, (String, crate::models::SenderCheck, bool)>,
    async_imap::error::Error,
> {
    with_one_retry(session, account, async |s| load_bodies(s, path, uids).await).await
}

/// [`load_source`] with [`load_body_retry`]'s one reconnect-and-retry.
pub(super) async fn load_source_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<String, async_imap::error::Error> {
    with_one_retry(session, account, async |s| load_source(s, path, uid).await).await
}

/// [`load_raw`] with [`load_body_retry`]'s one reconnect-and-retry.
pub(super) async fn load_raw_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, async_imap::error::Error> {
    with_one_retry(session, account, async |s| load_raw(s, path, uid).await).await
}
