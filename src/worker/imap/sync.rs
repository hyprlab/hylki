//! Loading a folder's list: the newest window, merged over the cached index,
//! with the attachment flags reconciled.

use crate::cache::Cache;
use crate::models::Message;
use crate::worker::imap::fetch_mode::inline_previews_wanted;
use crate::worker::imap::flag_sync::{server_flag_changes, FlagState};
use crate::worker::imap::gmail::read_gmail_labels;
use crate::worker::imap::idle::note_synced_uid_next;
use crate::worker::imap::preview_fetch::{
    preview_fetch_items, preview_of, redecode_garbled_previews, retry_missing_previews,
    PREVIEW_REDECODE_ROWS,
};
use crate::worker::imap::summary::{build_summary, summary_from_headers};
use crate::worker::imap::wire::{logged_fetch, search_uids, sel};
use crate::worker::imap::{ImapSession, FIRST_PAGE, PAGE_SIZE, REFS_FETCH_ITEM};

/// Load a folder's message index for immediate display.
///
/// Never-synced folder → fetch a fast [`FIRST_PAGE`] of the newest messages so
/// browsing is instant; the background backfill indexes the rest. Already-cached
/// folder → fetch just the recent window and merge it over the existing (possibly
/// whole-mailbox) index, picking up new mail and flag changes without re-pulling
/// thousands of envelopes.
pub(in crate::worker) async fn load_messages(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    path: &str,
    use_envelope: bool,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    note_synced_uid_next(account_id, path, mailbox.uid_next);
    let total = mailbox.exists;
    if total == 0 {
        // Folder emptied on the server — drop any cached copies so they don't linger.
        if let Some(c) = cache {
            for uid in c.cached_uids(account_id, path) {
                c.delete_message(account_id, path, uid);
            }
        }
        return Ok(Vec::new());
    }

    let cached = cache
        .map(|c| c.load_messages(account_id, path, folder_id))
        .unwrap_or_default();
    if cached.is_empty() {
        let mut messages = fetch_window(
            account_id,
            session,
            folder_id,
            total,
            FIRST_PAGE,
            use_envelope,
        )
        .await?;
        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
        read_gmail_labels(account_id, session, path, &uids, &mut messages).await?;
        reconcile_attachment_flags(cache, account_id, path, &mut messages);
        Ok(messages)
    } else {
        let recent = fetch_window(
            account_id,
            session,
            folder_id,
            total,
            PAGE_SIZE,
            use_envelope,
        )
        .await?;
        let window: Vec<u32> = recent.iter().map(|m| m.uid).collect();
        let mut merged = merge_index(cached, recent);
        read_gmail_labels(account_id, session, path, &window, &mut merged).await?;
        redecode_garbled_previews(session, &mut merged).await;
        // Reconcile deletions/moves made on the server or another device: drop any
        // message whose UID the server no longer lists (a plain merge would keep it
        // forever), and prune it from the cache so it doesn't come back. The full
        // UID set is a cheap server-side search even for large mailboxes.
        let server: std::collections::HashSet<u32> = search_uids(session, "ALL").await?;
        if let Some(c) = cache {
            for m in merged.iter().filter(|m| !server.contains(&m.uid)) {
                c.delete_message(account_id, path, m.uid);
            }
        }
        merged.retain(|m| server.contains(&m.uid));
        reconcile_older_flags(session, &window, &mut merged).await?;
        reconcile_attachment_flags(cache, account_id, path, &mut merged);
        Ok(merged)
    }
}

/// Stars and read state changed on another device for mail older than the
/// window just fetched, whose flags are the index's until now (#532). The
/// window's own flags are the server's already.
async fn reconcile_older_flags(
    session: &mut ImapSession,
    window: &[u32],
    merged: &mut [Message],
) -> Result<(), async_imap::error::Error> {
    let fresh: std::collections::HashSet<u32> = window.iter().copied().collect();
    let older: Vec<FlagState> = merged
        .iter()
        .filter(|m| !fresh.contains(&m.uid))
        .map(|m| FlagState {
            uid: m.uid,
            starred: m.starred,
            unread: m.unread,
        })
        .collect();
    let changes = server_flag_changes(session, &older).await?;
    if changes.is_empty() {
        return Ok(());
    }
    let at: std::collections::HashMap<u32, usize> =
        merged.iter().enumerate().map(|(i, m)| (m.uid, i)).collect();
    for s in changes {
        if let Some(&i) = at.get(&s.uid) {
            merged[i].starred = s.starred;
            merged[i].unread = s.unread;
        }
    }
    Ok(())
}

/// Clear the "has attachment" flag on freshly-fetched summaries whose bodies we
/// already downloaded and found to contain no real attachments. Server summary
/// flags (especially iCloud's header-only `multipart/mixed` guess) over-report
/// attachments for HTML mail whose only extra parts are inline `cid:` images.
fn reconcile_attachment_flags(
    cache: Option<&Cache>,
    account_id: u32,
    path: &str,
    messages: &mut [Message],
) {
    let Some(c) = cache else { return };
    if messages.iter().all(|m| !m.has_attachment) {
        return;
    }
    let attachmentless = c.attachmentless_uids(account_id, path);
    if attachmentless.is_empty() {
        return;
    }
    for m in messages.iter_mut() {
        if m.has_attachment && attachmentless.contains(&m.uid) {
            m.has_attachment = false;
        }
    }
}

/// Fetch the most-recent `count` messages' summaries (newest first). Includes
/// BODYSTRUCTURE so the attachment indicator is known for every indexed message
/// — still no bodies/attachments are downloaded.
async fn fetch_window(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    total: u32,
    count: u32,
    use_envelope: bool,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let start = total.saturating_sub(count - 1).max(1);
    let range = format!("{start}:{total}");

    // Normally we fetch the structured ENVELOPE + BODYSTRUCTURE (compact, and the
    // latter gives the attachment indicator). But some servers (notably iCloud)
    // emit RFC-noncompliant ENVELOPE/BODYSTRUCTURE (e.g. NIL transfer-encodings,
    // unescaped quotes in the Message-ID) that our IMAP parser rejects. For those
    // the caller retries with `use_envelope = false`, and we instead pull the raw
    // header block — opaque to the IMAP parser — and parse it with mail-parser.
    // `BODY.PEEK[1]<0.2048>` rides along for the list preview, with `BODY.PEEK[1.MIME]`
    // — the part's own headers — so the bytes are read in the charset they
    // declare rather than assumed UTF-8 (#159). Section 1 is the
    // first body part of a multipart message and the whole body of a single-part
    // one, so one query covers both without a second round trip per message.
    // When that first part is itself a multipart — `mixed(alternative(text, html),
    // attachment)`, which is what ProtonMail sends — what comes back is the nested
    // MIME rather than any text, and [`preview_from_part`] descends into it.
    //
    // With previews switched off it is left out altogether: the setting exists
    // partly to avoid downloading a slice of every message, so honouring it only
    // in the UI would miss the point. Read per sync, so turning it back on takes
    // effect without a restart.
    let want_preview = inline_previews_wanted(account_id);
    let preview_part = if want_preview {
        preview_fetch_items()
    } else {
        String::new()
    };
    let mut messages: Vec<Message> = if use_envelope {
        let query = format!(
            "(UID ENVELOPE FLAGS BODYSTRUCTURE INTERNALDATE{REFS_FETCH_ITEM}{preview_part})"
        );
        let fetches = logged_fetch(session, &range, &query).await?;
        fetches
            .iter()
            .map(|f| {
                let mut m = build_summary(account_id, f, folder_id);
                m.preview = preview_of(f);
                m
            })
            .collect()
    } else {
        let query = format!("(UID FLAGS BODY.PEEK[HEADER] INTERNALDATE{preview_part})");
        let fetches = logged_fetch(session, &range, &query).await?;
        fetches
            .iter()
            .map(|f| {
                let mut m = summary_from_headers(account_id, f, folder_id);
                m.preview = preview_of(f);
                m
            })
            .collect()
    };
    // Fill in previews the summary fetch came back without (see
    // [`retry_missing_previews`] for who those are). Bounded so body-less
    // messages don't grow the fetch each sync.
    if want_preview {
        let missing: Vec<u32> = messages
            .iter()
            .filter(|m| m.preview.is_empty())
            .take(PREVIEW_REDECODE_ROWS)
            .map(|m| m.uid)
            .collect();
        retry_missing_previews(session, &mut messages, missing).await;
    }
    messages.reverse(); // IMAP returns oldest-first; show newest at the top.
    Ok(messages)
}

/// Overlay a freshly-fetched recent window onto the cached index: recent rows
/// replace their cached versions (updated flags / new mail), the rest are kept.
/// No size cap — the whole folder is searchable once the background backfill has
/// indexed it.
///
/// A recent row that came back without a preview keeps the cached one, as
/// [`Cache::upsert_messages`] does. Otherwise the list this returns never
/// matched the one served from the cache a moment earlier, and every quiet
/// sync looked to the app like new mail (#330).
fn merge_index(cached: Vec<Message>, recent: Vec<Message>) -> Vec<Message> {
    let mut map: std::collections::HashMap<u32, Message> =
        cached.into_iter().map(|m| (m.uid, m)).collect();
    for mut m in recent {
        if let Some(old) = map.get_mut(&m.uid) {
            if m.preview.is_empty() {
                m.preview = std::mem::take(&mut old.preview);
            }
            // The summary fetch carries no Gmail labels: they are read after
            // the merge, and a message they could not be read for keeps the
            // ones it had.
            if m.labels.is_empty() {
                m.labels = std::mem::take(&mut old.labels);
            }
        }
        map.insert(m.uid, m);
    }
    let mut out: Vec<Message> = map.into_values().collect();
    out.sort_by_key(|m| std::cmp::Reverse(m.uid)); // newest first
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::test_support::flagged_message;

    /// A sync whose recent window came back without some previews must give
    /// the same list the cache served, or every quiet poll redraws the list
    /// (#330).
    #[test]
    fn merging_a_recent_window_keeps_the_cached_previews() {
        let cached: Vec<Message> = (1..=3)
            .map(|uid| Message {
                preview: format!("text {uid}"),
                ..flagged_message(uid)
            })
            .collect();
        let recent = vec![
            Message {
                unread: false,
                ..flagged_message(3)
            },
            Message {
                preview: "new text".into(),
                ..flagged_message(2)
            },
        ];
        let merged = merge_index(cached, recent);
        let previews: Vec<&str> = merged.iter().map(|m| m.preview.as_str()).collect();
        assert_eq!(previews, ["text 3", "new text", "text 1"]);
        assert!(!merged[0].unread, "the server's flags still win");
    }

    /// The summary fetch never carries Gmail labels (#374): a merge keeps
    /// the cached ones until the label read replaces them.
    #[test]
    fn merging_a_recent_window_keeps_the_cached_labels() {
        let cached: Vec<Message> = (1..=2)
            .map(|uid| Message {
                labels: vec![format!("Label {uid}")],
                ..flagged_message(uid)
            })
            .collect();
        let merged = merge_index(cached, vec![flagged_message(2), flagged_message(3)]);
        let labels: Vec<Vec<String>> = merged.iter().map(|m| m.labels.clone()).collect();
        assert_eq!(
            labels,
            [
                vec![],
                vec!["Label 2".to_string()],
                vec!["Label 1".to_string()]
            ]
        );
    }
}
