//! Fetching whole messages: the raw bytes three ways (#226), bodies one at
//! a time or in batches, and the source view.

use async_imap::types::Fetch;
use futures::TryStreamExt;

use crate::cache::Cache;
use crate::worker::imap::wire::{fetch_uids, search_uids, select_for_read, uid_set};
use crate::worker::imap::ImapSession;
use crate::worker::{render_raw, WorkerEvent};

/// Account for conversation members a body fetch came back without.
///
/// Either the message is no longer in the folder, or the server lists it and
/// will not hand it over (#226). The cache cannot tell: Gmail takes every
/// label off mail moved to Trash, so the copies cached under All Mail and
/// the other labels outlive it there, and a reply to a deleted conversation
/// brought them all back as blank cards (#257). The server is asked which
/// of the UIDs it still has. The ones it does not are dropped from the
/// cache and the reader is told they are gone; the rest keep the
/// placeholder, which says in place that the server gave nothing.
pub(super) async fn settle_missing_bodies(
    session: &mut Option<ImapSession>,
    account_id: u32,
    path: &str,
    missing: Vec<(u32, u32)>,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let uids: Vec<u32> = missing.iter().map(|(_, uid)| *uid).collect();
    // The folder is still selected from the fetch.
    let present = match session.as_mut() {
        Some(sess) => search_uids(sess, format!("UID {}", uid_set(&uids)))
            .await
            .ok(),
        None => None,
    };
    for (message_id, uid) in missing {
        if present.as_ref().is_some_and(|p| !p.contains(&uid)) {
            if let Some(c) = cache {
                c.delete_message(account_id, path, uid);
            }
            emit(WorkerEvent::Gone {
                message_id,
                path: path.to_string(),
                uid,
            });
        } else {
            emit(WorkerEvent::Body {
                message_id,
                path: path.to_string(),
                body: "(empty message)".to_string(),
            });
        }
    }
}

/// Fetch the raw RFC 822 source (headers + body) of a message, undecoded.
pub(super) async fn load_source(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<String, async_imap::error::Error> {
    select_for_read(session, path).await?;
    let raw = fetch_raw_message(session, uid)
        .await?
        .ok_or_else(|| no_content_error(uid))?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// Fetch the raw RFC 822 bytes of a message (binary-safe, for attachments).
/// The whole message out of one FETCH item, however the server labelled
/// it: `BODY[]` or `RFC822` as asked, or the header and text as two items,
/// which is what a server that will not hand over `BODY[]` (#226) can
/// still be asked for, joined back into one message.
fn raw_of(f: &Fetch) -> Option<Vec<u8>> {
    if let Some(b) = f.body() {
        return Some(b.to_vec());
    }
    match (f.header(), f.text()) {
        (Some(h), Some(t)) => {
            let mut raw = h.to_vec();
            if !raw.ends_with(b"\r\n\r\n") {
                if raw.ends_with(b"\r\n") {
                    raw.extend_from_slice(b"\r\n");
                } else {
                    raw.extend_from_slice(b"\r\n\r\n");
                }
            }
            raw.extend_from_slice(t);
            Some(raw)
        }
        (None, Some(t)) => Some(t.to_vec()),
        _ => None,
    }
}

/// Fetch one message's raw bytes, asking three ways before giving up: the
/// usual `BODY.PEEK[]`, then `RFC822`, then the header and text as two
/// items. Mailfence answered the first with a FETCH that carried no
/// message at all (#226), which read as an empty message and was shown,
/// and cached, as one. `None` means the server would not hand it over any
/// way; the reply's shape is logged so the export says what came back.
async fn fetch_raw_message(
    session: &mut ImapSession,
    uid: u32,
) -> Result<Option<Vec<u8>>, async_imap::error::Error> {
    for (n, items) in [
        "(BODY.PEEK[])",
        "(RFC822)",
        "(BODY.PEEK[HEADER] BODY.PEEK[TEXT])",
    ]
    .iter()
    .enumerate()
    {
        let fetches: Vec<Fetch> = fetch_uids(session, uid.to_string(), items)
            .await?
            .try_collect()
            .await?;
        if let Some(raw) = fetches.iter().find_map(raw_of) {
            if n > 0 {
                tracing::info!(target: "hylki::imap", "message {uid}: content came with {items}");
            }
            return Ok(Some(raw));
        }
        tracing::warn!(
            target: "hylki::imap",
            "message {uid}: {} FETCH item(s) came back for {items}, none carrying the message: {}",
            fetches.len(),
            fetches.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>().join(" | ").chars().take(600).collect::<String>()
        );
    }
    Ok(None)
}

/// The error for a message the server would not hand over (#226): surfaced
/// rather than shown as empty, and never cached.
fn no_content_error(uid: u32) -> async_imap::error::Error {
    async_imap::error::Error::Bad(format!(
        "the server sent no content for message {uid} (asked as BODY.PEEK[], RFC822, and header plus text)"
    ))
}

pub(super) async fn load_raw(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, async_imap::error::Error> {
    select_for_read(session, path).await?;
    fetch_raw_message(session, uid)
        .await?
        .ok_or_else(|| no_content_error(uid))
}

/// Returns the rendered body, the sender verdict, and whether the message
/// actually carries attachments — all three from the one fetch.
pub(in crate::worker) async fn load_body(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<(String, crate::models::SenderCheck, bool), async_imap::error::Error> {
    select_for_read(session, path).await?;

    // Fetch the whole message (PEEK so \Seen isn't set) and extract the body with
    // mail-parser. We deliberately avoid a BODYSTRUCTURE-based "text part only"
    // fast path: some servers (iCloud) return structures our IMAP parser rejects,
    // which would fail the fetch and corrupt the session.
    // The whole message is in hand, so the sender check rides along for free
    // rather than costing a second fetch. The paperclip is guessed from
    // BODYSTRUCTURE (or, on servers whose structure we can't parse, from the
    // top-level Content-Type), and both guesses miss shapes like Apple Mail's
    // inline PDF nested under an alternative (issue #9). The whole message is
    // in hand here, so the guess can be replaced with fact, at no extra
    // network cost. A server that hands nothing over is an error, not an
    // empty message (#226): shown as one, it read as the message being
    // blank, and was cached as such.
    let raw = fetch_raw_message(session, uid)
        .await?
        .ok_or_else(|| no_content_error(uid))?;
    Ok(render_raw(&raw))
}

/// Fetch several messages' bodies from one folder in a single `uid_fetch`.
///
/// The per-message extraction is [`load_body`]'s, applied to each response in
/// the set: body, sender check and the attachment fact all come off the raw
/// message that is already in hand, at no extra network cost. A UID the server
/// doesn't answer is simply absent from the map — the caller decides what to
/// show in its place.
pub(super) async fn load_bodies(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
) -> Result<
    std::collections::HashMap<u32, (String, crate::models::SenderCheck, bool)>,
    async_imap::error::Error,
> {
    Ok(load_raws(session, path, uids)
        .await?
        .into_iter()
        .map(|(uid, raw)| (uid, render_raw(&raw)))
        .collect())
}

/// Several messages' raw sources from one folder in a single `uid_fetch`,
/// by UID. A UID the server doesn't answer is absent from the map.
pub(super) async fn load_raws(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
) -> Result<std::collections::HashMap<u32, Vec<u8>>, async_imap::error::Error> {
    select_for_read(session, path).await?;
    let set = uid_set(uids);
    let fetches: Vec<Fetch> = fetch_uids(session, set, "(BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;
    let mut out = std::collections::HashMap::new();
    let mut last_uid: Option<u32> = None;
    for f in &fetches {
        if let Some(u) = f.uid {
            last_uid = Some(u);
        }
        // A message's response can arrive split across items — [`load_body`]
        // scans every one of them for the body rather than trusting the first,
        // and this has to be as forgiving. An item with no body is not an empty
        // message, just not the part carrying one, so it is skipped: leaving the
        // UID out of the map means the caller shows "(empty message)" without
        // writing that over a real body in the cache.
        let (Some(uid), Some(raw)) = (last_uid, raw_of(f)) else {
            continue;
        };
        out.entry(uid).or_insert(raw);
    }
    Ok(out)
}
