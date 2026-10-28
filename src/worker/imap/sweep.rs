//! Chores that ride the idle connection: the unread-count sweep over every
//! folder, auto-empty (#140), list clean-up (#407), and body search hits
//! for filters (#191).

use async_imap::types::Fetch;
use futures::TryStreamExt;
use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{FolderKind, Message};
use crate::worker::chores::{
    auto_empty_due, auto_empty_roles, clean_lists_due, clean_lists_query, imap_before_date,
    role_folder_path,
};
use crate::worker::imap::folders::{is_inbox_path, refresh_folders};
use crate::worker::imap::mutate::{move_messages, purge_messages};
use crate::worker::imap::summary::fetched_fields;
use crate::worker::imap::wire::{exam, fetch_uids, search_uids, sel, uid_set};
use crate::worker::imap::ImapSession;
use crate::worker::{chip_counts_all, MailRequest, WorkerEvent};

/// Auto-empty (#140) for an IMAP account: delete for good whatever in the
/// Junk / Trash folder is older than the account's chosen age. Rides the
/// unread sweep, at most every [`AUTO_EMPTY_MIN`]. Age is the message's
/// internal date, the day it reached the server, which is how the other
/// clients count too. A failure is logged and tried again next time; the
/// sweep never surfaces an error for a chore nobody is waiting on.
pub(super) async fn auto_empty_imap(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !auto_empty_due(account, last) {
        return;
    }
    let folders = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let mut purged_any = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let Some(path) = role_folder_path(account, &folders, kind, role) else {
            tracing::info!("auto-empty: account {account_id} has no {role} folder");
            continue;
        };
        let before = imap_before_date(days);
        let found = match sel(session, &path).await {
            Ok(_) => search_uids(session, format!("BEFORE {before}")).await,
            Err(e) => Err(e),
        };
        let uids: Vec<u32> = match found {
            Ok(set) => set.into_iter().collect(),
            Err(e) => {
                tracing::warn!("auto-empty: could not search {path} on account {account_id}: {e}");
                continue;
            }
        };
        if uids.is_empty() {
            continue;
        }
        match purge_messages(session, &path, &uids).await {
            Ok(()) => {
                if let Some(c) = cache {
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
                tracing::info!(
                    "auto-empty: deleted {} message(s) older than {days} days from {path} on account {account_id}",
                    uids.len()
                );
                purged_any = true;
            }
            Err(e) => {
                tracing::warn!(
                    "auto-empty: could not delete from {path} on account {account_id}: {e}"
                );
            }
        }
    }
    if purged_any {
        refresh_folders(account_id, account, session, cache, emit).await;
    }
}

/// Which of `uids` (in the selected folder) carry a List-Unsubscribe
/// header, read 500 at a time.
async fn with_list_header(
    session: &mut ImapSession,
    uids: &[u32],
) -> Result<Vec<u32>, async_imap::error::Error> {
    let mut out = Vec::new();
    for chunk in uids.chunks(500) {
        let fetches: Vec<Fetch> = fetch_uids(
            session,
            uid_set(chunk),
            "(UID BODY.PEEK[HEADER.FIELDS (LIST-UNSUBSCRIBE)])",
        )
        .await?
        .try_collect()
        .await?;
        out.extend(fetches.iter().filter_map(|f| {
            let listed = fetched_fields(f).iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("list-unsubscribe") && !value.is_empty()
            });
            listed.then_some(f.uid?)
        }));
    }
    Ok(out)
}

/// Move the Inbox's list mail older than the account's chosen age to the
/// Trash (#407), searched for on the server, so mail never opened counts
/// too. It rides the unread sweep like auto-empty, and a failure is logged
/// and tried again next time. The Inbox list is redrawn from the cache.
pub(super) async fn clean_lists_imap(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !clean_lists_due(account, last) {
        return;
    }
    let days = account.clean_lists_days;
    let folders = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let Some(inbox) = folders
        .iter()
        .find(|f| f.kind == FolderKind::Inbox)
        .cloned()
    else {
        return;
    };
    let Some(trash) = role_folder_path(account, &folders, FolderKind::Trash, "trash") else {
        tracing::info!("list clean-up: account {account_id} has no Trash folder");
        return;
    };
    let found = match sel(session, &inbox.path).await {
        Ok(_) => search_uids(session, clean_lists_query(days)).await,
        Err(e) => Err(e),
    };
    let found = match found {
        Ok(set) => {
            let mut candidates: Vec<u32> = set.into_iter().collect();
            candidates.sort_unstable();
            with_list_header(session, &candidates).await
        }
        Err(e) => Err(e),
    };
    let uids: Vec<u32> = match found {
        Ok(uids) => uids,
        Err(e) => {
            tracing::warn!(
                "list clean-up: could not search {} on account {account_id}: {e}",
                inbox.path
            );
            return;
        }
    };
    if uids.is_empty() {
        return;
    }
    match move_messages(session, &inbox.path, &uids, &trash).await {
        Ok(_) => {
            tracing::info!(
                "list clean-up: moved {} list message(s) older than {days} days to {trash} on account {account_id}",
                uids.len()
            );
            if let Some(c) = cache {
                for uid in &uids {
                    c.delete_message(account_id, &inbox.path, *uid);
                }
                emit(WorkerEvent::Messages {
                    folder_id: inbox.id,
                    messages: c.load_messages(account_id, &inbox.path, inbox.id),
                });
            }
        }
        Err(e) => {
            tracing::warn!("list clean-up: could not move list mail on account {account_id}: {e}")
        }
    }
}

/// Re-count every folder's unread chip (EXAMINE + SEARCH each), one folder
/// at a time — and only while nothing is waiting on the worker. Each folder
/// costs two round trips, so a dozen folders on a slow link hold the
/// session for seconds; a message the user just opened would wait behind
/// the whole walk (a notification click did exactly that). `pending` is the
/// walk's place: empty starts a fresh pass over the cached folder list, and
/// what is left when a request interrupts is resumed by the next call.
/// Returns the folders whose counts moved and whether the pass finished.
///
/// Sweep every known folder for its true unread count and emit a
/// [`WorkerEvent::FolderUnread`] for each — the per-folder event is the one
/// path allowed to assert a genuine zero (the merged Folders path deliberately
/// won't, see the app's SetFolders). A folder that can't be read emits
/// nothing, so a wedged answer can't wipe a good chip.
///
/// EXAMINE + UID SEARCH, not STATUS: STATUS answers from a cache that lags
/// minutes behind on some servers (iCloud, see the inbox refinement in
/// [`list_folders`]) — stale numbers left chips unmoved and, worse, blinded
/// the change detection below. The read-only EXAMINE walk costs a second
/// round trip per folder and leaves the session's selection wherever it ends;
/// subsequent reads check that selection and mutations re-select explicitly.
///
/// `selected` (the mailbox the main loop is working) is skipped: its count is
/// already refreshed by every load and IDLE pass, and examining it out from
/// under the loop would be rude.
///
/// Doubles as the activity detector for the dynamic watchers: each folder's
/// (unseen, UIDNEXT) pair is compared against `baseline` — the same pair from
/// the previous sweep — and the folders that moved are returned so the caller
/// can put them on the watch list. UIDNEXT (from the EXAMINE response)
/// catches deliveries of already-read mail that the unseen count alone would
/// miss. A folder with no baseline yet is recorded, not reported: everything
/// that happened while Hylki was closed would otherwise look like fresh
/// activity.
pub(super) async fn refresh_unread_counts(
    account_id: u32,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    selected: Option<&str>,
    baseline: &mut std::collections::HashMap<String, (u32, Option<u32>)>,
    pending: &mut Vec<crate::models::Folder>,
    rx: &mpsc::UnboundedReceiver<MailRequest>,
    emit: &impl Fn(WorkerEvent),
) -> (Vec<(FolderKind, String)>, bool) {
    let mut changed = Vec::new();
    if pending.is_empty() {
        *pending = cache
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default();
        pending.reverse(); // popped from the back, so walked in list order
    }
    while let Some(f) = pending.last().cloned() {
        if !rx.is_empty() {
            tracing::debug!("sweep: paused with {} folders left", pending.len());
            return (changed, false);
        }
        pending.pop();
        if Some(f.path.as_str()) == selected {
            continue;
        }
        let Ok(mb) = exam(session, &f.path).await else {
            tracing::debug!("sweep: cannot examine {}", f.path);
            continue;
        };
        if f.kind == FolderKind::Snoozed {
            emit(WorkerEvent::SnoozedCount {
                path: f.path.clone(),
                count: mb.exists,
            });
        }
        let Some(unread) = selected_chip_count(session, Some(f.kind)).await else {
            continue;
        };
        emit(WorkerEvent::FolderUnread {
            folder_id: f.id,
            unread,
        });
        let now = (unread, mb.uid_next);
        match baseline.insert(f.path.clone(), now) {
            Some(prev) if prev != now => {
                tracing::debug!("sweep: {} moved {:?} -> {:?}", f.path, prev, now);
                changed.push((f.kind, f.path.clone()));
            }
            _ => {}
        }
    }
    if !changed.is_empty() {
        tracing::info!(
            "sweep: changed {:?}",
            changed.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>()
        );
    }
    (changed, true)
}

/// The chip number for the currently-selected mailbox: unseen, or every
/// message for a kind that counts them all. `None` for an unknown kind
/// counts unseen.
pub(super) async fn selected_chip_count(
    session: &mut ImapSession,
    kind: Option<FolderKind>,
) -> Option<u32> {
    if kind.is_some_and(chip_counts_all) {
        search_uids(session, "ALL")
            .await
            .ok()
            .map(|uids| uids.len() as u32)
    } else {
        selected_unseen(session).await
    }
}

/// Answer the "Message body" filter conditions (#191) for an inbox sync on
/// the server: one `UID SEARCH … BODY` per alternative over the listed
/// uids, so the rules learn what the server finds without a body coming
/// down. The answer goes out as [`WorkerEvent::BodyHits`] just ahead of the
/// messages. Nothing is sent for other folders, without body rules, or for
/// an empty list; a search the server refuses is logged and skipped, and
/// that alternative then works off the list preview alone.
pub(in crate::worker) async fn emit_body_hits(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    messages: &[Message],
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if messages.is_empty() || !is_inbox_path(cache, account_id, path) {
        return;
    }
    let needles = crate::config::filter_body_needles(&account.email);
    if needles.is_empty() {
        return;
    }
    let Some(sess) = session.as_mut() else { return };
    let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
    let listed: std::collections::HashSet<u32> = uids.iter().copied().collect();
    let set = uid_set(&uids);
    let mut hits: std::collections::HashMap<u32, Vec<String>> = Default::default();
    for needle in needles {
        let query = body_search_query(&set, &needle);
        match search_uids(sess, &query).await {
            Ok(found) => {
                for uid in found.into_iter().filter(|u| listed.contains(u)) {
                    hits.entry(uid).or_default().push(needle.clone());
                }
            }
            Err(e) => tracing::warn!("filter: body search for {needle:?} failed: {e}"),
        }
    }
    tracing::info!(
        "filter: body search over {} messages hit {}",
        uids.len(),
        hits.len()
    );
    emit(WorkerEvent::BodyHits { folder_id, hits });
}

/// The `UID SEARCH` keys that find `needle` in the bodies of `set`. A
/// needle beyond ASCII needs the CHARSET key in front (RFC 3501 §6.4.4),
/// which must lead the keys; quotes and backslashes are escaped as in any
/// quoted string, and line breaks can never be in a quoted string at all.
fn body_search_query(set: &str, needle: &str) -> String {
    let clean: String = needle
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n'))
        .collect();
    let quoted = clean.replace('\\', "\\\\").replace('"', "\\\"");
    let charset = if clean.is_ascii() {
        ""
    } else {
        "CHARSET UTF-8 "
    };
    format!("{charset}UID {set} BODY \"{quoted}\"")
}

/// Count unseen messages in the currently-selected mailbox via SEARCH (safe on
/// the selected folder, unlike STATUS on some servers). Downloads only ids.
pub(super) async fn selected_unseen(session: &mut ImapSession) -> Option<u32> {
    search_uids(session, "UNSEEN")
        .await
        .ok()
        .map(|uids| uids.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_search_keys_quote_and_declare_charset() {
        // #191: ASCII needles go as a plain quoted string over the listed
        // uids; quotes and backslashes are escaped; anything beyond ASCII
        // puts CHARSET UTF-8 at the head of the keys; line breaks vanish.
        assert_eq!(
            body_search_query("1:5,9", "unsubscribe"),
            r#"UID 1:5,9 BODY "unsubscribe""#
        );
        assert_eq!(
            body_search_query("1", r#"say "hi" \now"#),
            r#"UID 1 BODY "say \"hi\" \\now""#
        );
        assert_eq!(
            body_search_query("1", "café"),
            r#"CHARSET UTF-8 UID 1 BODY "café""#
        );
        assert_eq!(body_search_query("1", "a\r\nb"), r#"UID 1 BODY "ab""#);
    }
}
