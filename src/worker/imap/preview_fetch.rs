//! Asking the server for list previews: the first bytes of the first body
//! part with its own headers, and repairing previews that came out wrong.

use async_imap::types::Fetch;
use futures::TryStreamExt;

use crate::models::Message;
use crate::worker::imap::fetch_mode::inline_previews_wanted;
use crate::worker::imap::wire::{fetch_uids, uid_set};
use crate::worker::imap::ImapSession;
use crate::worker::preview::{
    charset_param, finish_preview, mime_header, preview_from_part, text_in_multipart, text_like,
};

/// What the BODY[TEXT] retry found for a row, as (account, folder id, uid) →
/// preview, kept for the session. The summary fetch comes back empty for
/// the same rows on every sync (iCloud's re-appended messages, file-first
/// messages), so without this the retry ran again each time — three rows
/// cost ~100 KB per sync in one user's log. An empty string is a message
/// with no text to show at all — an encrypted one, a photo — which is then
/// not asked about again either.
static PREVIEW_RETRIED: std::sync::LazyLock<std::sync::Mutex<RetriedPreviews>> =
    std::sync::LazyLock::new(Default::default);

type RetriedPreviews = std::collections::HashMap<(u32, u32, u32), String>;

/// The preview snippet from a fetch that asked for `BODY.PEEK[1]` together with
/// `BODY.PEEK[1.MIME]`, the part's own headers, which say what the bytes are.
pub(super) fn preview_of(fetch: &Fetch) -> String {
    use async_imap::imap_proto::types::SectionPath;
    let ctype = section_content_type(fetch);
    // An image or a document as the first part — photo mail, a forwarded
    // file — has no text to show. An empty preview here lets the BODY[TEXT]
    // retry look further into the message for the text part, instead of the
    // row wearing the file's first bytes.
    if ctype.as_deref().is_some_and(|t| !text_like(t)) {
        return String::new();
    }
    let charset = ctype.as_deref().and_then(charset_param);
    let encoding = section_encoding(fetch);
    let bytes = fetch
        .section(&SectionPath::Part(vec![1], None))
        .unwrap_or_default();
    let p = preview_from_part(bytes, charset.as_deref(), encoding.as_deref());
    // A preheader is built to defeat exactly this read: a marketing mail can
    // spend thousands of bytes on invisible padding before its first real
    // sentence, the way an HTML part spends them on <head>. When the slice ran
    // to its limit and still yielded almost nothing to show, report nothing
    // rather than a scrap, and let the deeper repair read — bounded, and rare
    // — go and find the text.
    if bytes.len() >= PREVIEW_FETCH_BYTES && p.chars().count() < PREVIEW_MIN_CHARS {
        return String::new();
    }
    // GTK labels abort on interior NULs, and decoded message text can carry
    // them.
    if p.contains('\0') {
        p.replace('\0', " ")
    } else {
        p
    }
}

/// The lowercased Content-Type section 1 declares, read from the
/// `BODY[1.MIME]` header block that rides along with the preview fetch (#159).
/// `None` when the server sent no MIME headers.
fn section_content_type(fetch: &Fetch) -> Option<String> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let mime = fetch.section(&SectionPath::Part(vec![1], Some(MessageSection::Mime)))?;
    mime_header(&String::from_utf8_lossy(mime), "content-type")
}

/// The charset section 1 declares, when its headers name one — a multipart
/// part 1 doesn't, its text parts carry their own.
fn section_charset(fetch: &Fetch) -> Option<String> {
    section_content_type(fetch)
        .as_deref()
        .and_then(charset_param)
}

/// The transfer encoding section 1 declares, from the same `BODY[1.MIME]`
/// block the charset comes from. Reading it beats guessing: a part can be
/// quoted-printable and still be spelled entirely in base64's alphabet.
fn section_encoding(fetch: &Fetch) -> Option<String> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let mime = fetch.section(&SectionPath::Part(vec![1], Some(MessageSection::Mime)))?;
    mime_header(&String::from_utf8_lossy(mime), "content-transfer-encoding")
}

/// How much of a message's first body part to fetch for the list preview. Enough
/// for a couple of lines of text after decoding, small enough that syncing a
/// large mailbox doesn't turn into downloading it.
const PREVIEW_FETCH_BYTES: usize = 2048;

/// Below this many characters, a preview read from a full slice has not found
/// the message's text yet — it is still in the preheader padding or the
/// `<head>` — and the repair read is worth the round trip.
const PREVIEW_MIN_CHARS: usize = 30;

/// The bigger slice for the preview *repair* fetch (the BODY[TEXT] retry for
/// messages whose summary fetch produced no preview). An HTML-only message can
/// spend several KB on `<head>` boilerplate before its first visible text —
/// which is exactly how a message ends up on the repair list — so the retry
/// reads deep enough to find it. Only a handful of messages ever qualify.
const PREVIEW_REPAIR_BYTES: usize = 32768;

/// How many cached rows with a garbled preview one sync re-reads.
pub(super) const PREVIEW_REDECODE_ROWS: usize = 12;

/// The fetch items that read a message's first part for the list preview: a
/// slice of its bytes plus its MIME headers, for the charset.
pub(super) fn preview_fetch_items() -> String {
    format!(" BODY.PEEK[1]<0.{PREVIEW_FETCH_BYTES}> BODY.PEEK[1.MIME]")
}

/// Re-read the previews of cached rows that still show replacement characters
/// (#159). Rows indexed before the preview fetch learned the part's charset
/// keep their garbled snippet otherwise, since only the recent window is read
/// again on each sync. A few rows per sync.
pub(super) async fn redecode_garbled_previews(session: &mut ImapSession, messages: &mut [Message]) {
    let Some(account_id) = messages.first().map(|m| m.account_id) else {
        return;
    };
    if !inline_previews_wanted(account_id) {
        return;
    }
    // Markup too (#683): a row cached with a preheader's entities holds
    // little else, and the real text is a re-read away. A preview read
    // since never carries any, so a row is re-read once.
    let uids: Vec<u32> = messages
        .iter()
        .filter(|m| {
            m.preview.contains('\u{fffd}') || crate::mail::preview_text::has_markup(&m.preview)
        })
        .take(PREVIEW_REDECODE_ROWS)
        .map(|m| m.uid)
        .collect();
    if uids.is_empty() {
        return;
    }
    tracing::info!("previews: re-reading garbled rows uids={uids:?}");
    let fetched: Result<Vec<Fetch>, _> = async {
        fetch_uids(
            session,
            uid_set(&uids),
            format!("(UID{})", preview_fetch_items()),
        )
        .await?
        .try_collect()
        .await
    }
    .await;
    let fetches = match fetched {
        Err(e) => {
            tracing::warn!("previews: garbled-row re-read failed: {e}");
            return;
        }
        Ok(f) => f,
    };
    let mut still_missing = Vec::new();
    for f in &fetches {
        let Some(m) = f
            .uid
            .and_then(|uid| messages.iter_mut().find(|m| m.uid == uid))
        else {
            continue;
        };
        // Whatever this read produced — including nothing, when the part
        // turned out to be a file — beats the garbage it replaces, and an
        // empty row is not asked about again next sync.
        m.preview = preview_of(f);
        if m.preview.is_empty() {
            still_missing.push(m.uid);
        }
    }
    retry_missing_previews(session, messages, still_missing).await;
}

/// Fill in previews for `uids`, rows whose first-part read came back empty.
///
/// iCloud answers BODY[1] with nothing on a message it re-appended (a move-back
/// from an undo, for one) — and keeps doing so on every later sync, so the row
/// would wear a blank summary for good — and a message whose first part is a
/// file has its text further in. The TEXT section survives (the full-body
/// fetch the reader uses works fine), so re-ask with that and run the same
/// MIME-prefix extraction. Callers bound the list: genuinely body-less messages
/// stay empty and shouldn't grow the fetch each sync.
pub(super) async fn retry_missing_previews(
    session: &mut ImapSession,
    messages: &mut [Message],
    uids: Vec<u32>,
) {
    // Rows the retry already answered this session are served from that
    // answer (see [`PREVIEW_RETRIED`]): the retry runs on every sync of the
    // folder, and it would otherwise cost 32 KB per row per sync for as
    // long as the row stayed among those checked.
    fn key(messages: &[Message], uid: u32) -> Option<(u32, u32, u32)> {
        messages
            .iter()
            .find(|m| m.uid == uid)
            .map(|m| (m.account_id, m.folder_id, uid))
    }
    let uids: Vec<u32> = {
        let retried = PREVIEW_RETRIED.lock().ok();
        uids.into_iter()
            .filter(|&uid| {
                let Some(k) = key(messages, uid) else {
                    return false;
                };
                match retried.as_ref().and_then(|r| r.get(&k)) {
                    None => true,
                    Some(found) => {
                        if let Some(m) = messages.iter_mut().find(|m| m.uid == uid) {
                            m.preview = found.clone();
                        }
                        false
                    }
                }
            })
            .collect()
    };
    if uids.is_empty() {
        return;
    }
    retry_missing_previews_now(session, messages, &uids).await;
    let answers: Vec<((u32, u32, u32), String)> = uids
        .iter()
        .filter_map(|&uid| {
            let k = key(messages, uid)?;
            let preview = messages.iter().find(|m| m.uid == uid)?.preview.clone();
            Some((k, preview))
        })
        .collect();
    if let Ok(mut retried) = PREVIEW_RETRIED.lock() {
        retried.extend(answers);
    }
}

/// The BODY[TEXT] retry itself, for `uids` (non-empty).
async fn retry_missing_previews_now(
    session: &mut ImapSession,
    messages: &mut [Message],
    uids: &[u32],
) {
    tracing::info!("previews: retrying via BODY[TEXT] uids={uids:?}");
    let refetched: Result<Vec<Fetch>, _> = async {
        fetch_uids(
            session,
            uid_set(uids),
            format!("(UID BODY.PEEK[TEXT]<0.{PREVIEW_REPAIR_BYTES}> BODY.PEEK[1.MIME])"),
        )
        .await?
        .try_collect()
        .await
    }
    .await;
    match refetched {
        Err(e) => tracing::warn!("previews: BODY[TEXT] retry failed: {e}"),
        Ok(fetches) => {
            use async_imap::imap_proto::types::{MessageSection, SectionPath};
            for f in &fetches {
                // For a multipart, `text_in_multipart` reads each part's own
                // headers and these go unused; for a single-part message,
                // section 1 is the body and they describe it exactly.
                let charset = section_charset(f);
                let encoding = section_encoding(f);
                // Section 1 is a file, not text (#241: a DMARC report that is
                // nothing but a zip). The summary fetch blanked the preview
                // for that reason, and this read must not undo it by decoding
                // the file: only a multipart body with a text part inside
                // has anything to show.
                let binary_first = section_content_type(f).is_some_and(|t| !text_like(&t));
                let p = f
                    .section(&SectionPath::Full(MessageSection::Text))
                    .map(|b| {
                        if binary_first {
                            text_in_multipart(b, 0)
                                .map(finish_preview)
                                .unwrap_or_default()
                        } else {
                            preview_from_part(b, charset.as_deref(), encoding.as_deref())
                        }
                    })
                    .unwrap_or_default()
                    .replace('\0', " ");
                if p.is_empty() {
                    continue;
                }
                if let Some(m) = f
                    .uid
                    .and_then(|uid| messages.iter_mut().find(|m| m.uid == uid))
                {
                    m.preview = p;
                }
            }
        }
    }
}
