//! Changing mail on the server: flags and keywords, moves, deleting for
//! good, folders, and spam reports.

use async_imap::types::{Fetch, Flag};
use futures::TryStreamExt;

use crate::worker::imap::wire::{
    copy_uids, create_box, delete_box, expunge_all, expunge_uids, imap_unquote, mv_uids,
    quote_mailbox, rename_box, sel, store_uids, subscribe_box, uid_set, unsubscribe_box,
};
use crate::worker::imap::ImapSession;

/// Permanently erase messages from a mailbox: flag them `\Deleted`, then expunge.
///
/// `UID EXPUNGE` (RFC 4315) removes only the UIDs we asked for, leaving alone
/// anything another client flagged `\Deleted` in the meantime. Servers without
/// UIDPLUS reject it, so fall back to a plain `EXPUNGE`.
pub(super) async fn purge_messages(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
) -> Result<(), async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(());
    }
    let set = uid_set(uids);
    sel(session, path).await?;
    flag_deleted_and_expunge(session, &set).await
}

/// Flag a UID set `\Deleted` and expunge it: `UID EXPUNGE` where the server
/// has UIDPLUS, a plain `EXPUNGE` otherwise.
pub(super) async fn flag_deleted_and_expunge(
    session: &mut ImapSession,
    set: &str,
) -> Result<(), async_imap::error::Error> {
    let _: Vec<Fetch> = store_uids(session, set, "+FLAGS (\\Deleted)")
        .await?
        .try_collect()
        .await?;
    // A server without UIDPLUS answers `BAD`, which surfaces while draining the
    // response stream rather than from the call itself — so judge it on the
    // collected result, not with `?`.
    let uid_expunged = match expunge_uids(session, set).await {
        Ok(stream) => stream.try_collect::<Vec<u32>>().await.is_ok(),
        Err(_) => false,
    };
    if !uid_expunged {
        let _: Vec<u32> = expunge_all(session).await?.try_collect().await?;
    }
    Ok(())
}

/// The IMAP flags a message moved in from another account (#265) keeps.
pub(super) fn import_flags(seen: bool, flagged: bool) -> Option<String> {
    let flags: Vec<&str> = [(seen, "\\Seen"), (flagged, "\\Flagged")]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| f)
        .collect();
    (!flags.is_empty()).then(|| format!("({})", flags.join(" ")))
}

/// The flags a rewritten message carries over: all but the ones the server
/// sets itself or that would delete it.
pub(super) fn kept_flags<'a>(flags: impl Iterator<Item = Flag<'a>>) -> Vec<String> {
    flags
        .filter_map(|f| match f {
            Flag::Seen => Some("\\Seen".to_string()),
            Flag::Answered => Some("\\Answered".to_string()),
            Flag::Flagged => Some("\\Flagged".to_string()),
            Flag::Draft => Some("\\Draft".to_string()),
            Flag::Custom(k) if !k.starts_with('\\') => Some(k.to_string()),
            _ => None,
        })
        .collect()
}

/// `UID MOVE` where the server offers it (RFC 6851), else what every client
/// did before there was a MOVE: `UID COPY`, flag the originals `\Deleted`,
/// expunge (issue #128 — a server without MOVE answers `UID MOVE` with
/// "command not permitted with UID", and every move, deleting included,
/// failed on it). The capability is asked for each time: it is one short
/// round trip, and a session's answer can change after login on some
/// servers, so it is not worth caching wrong.
pub(super) async fn uid_move(
    session: &mut ImapSession,
    set: &str,
    dest: &str,
) -> Result<(), async_imap::error::Error> {
    let can_move = match session.capabilities().await {
        Ok(caps) => caps.has_str("MOVE"),
        // If even CAPABILITY fails, let MOVE produce the real error.
        Err(_) => true,
    };
    tracing::debug!(target: "hylki::imap", "server {} MOVE", if can_move { "offers" } else { "lacks" });
    if can_move {
        return mv_uids(session, set, dest).await;
    }
    // `uid_copy` sends the mailbox name as given (unlike `uid_mv`, which
    // quotes it), so a name with a space — iCloud's "Deleted Messages", the
    // one server this path actually serves — went out bare and every move
    // and delete there answered "BAD Parse Error" (1.22.0 to 1.23.0).
    // `copy_uids` quotes it.
    copy_uids(session, set, dest).await?;
    flag_deleted_and_expunge(session, set).await
}

pub(super) async fn store_flag(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    flag: &str,
    add: bool,
) -> Result<(), async_imap::error::Error> {
    sel(session, path).await?;
    let op = if add { "+FLAGS" } else { "-FLAGS" };
    let query = format!("{op} ({flag})");
    // Drain the resulting FETCH stream so the command completes.
    let _: Vec<Fetch> = store_uids(session, uid.to_string(), query)
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Add or remove a user-defined keyword (a tag, #71). Answers whether the
/// server took it: `Ok(false)` means the mailbox's PERMANENTFLAGS lists no
/// `\*` (and not this keyword either), so a STORE would be refused or
/// forgotten at the end of the session — the caller keeps the tag locally.
/// A server that sends no PERMANENTFLAGS at all keeps every flag (RFC 3501).
pub(super) async fn store_keyword(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    keyword: &str,
    add: bool,
) -> Result<bool, async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    let allowed = mailbox.permanent_flags.is_empty()
        || mailbox.permanent_flags.iter().any(|f| match f {
            Flag::MayCreate => true,
            Flag::Custom(k) => k.eq_ignore_ascii_case(keyword),
            _ => false,
        });
    if !allowed {
        return Ok(false);
    }
    let op = if add { "+FLAGS" } else { "-FLAGS" };
    let query = format!("{op} ({keyword})");
    let _: Vec<Fetch> = store_uids(session, uid.to_string(), query)
        .await?
        .try_collect()
        .await?;
    Ok(true)
}

/// Move a message to `dest`, creating (and subscribing to) the destination
/// mailbox first if it doesn't exist yet. Returns whether a folder was created,
/// so the caller can refresh the folder list.
async fn move_or_create(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    if uid_move(session, &uid.to_string(), dest).await.is_ok() {
        return Ok(false);
    }
    // The move failed — most likely the destination mailbox doesn't exist (the
    // account has no Archive/Junk/… folder yet). Create it, subscribe, and retry;
    // if it still fails, surface that error.
    let created = create_box(session, dest).await.is_ok();
    let _ = subscribe_box(session, dest).await;
    sel(session, path).await?;
    uid_move(session, &uid.to_string(), dest).await?;
    Ok(created)
}

pub(super) async fn move_message(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    move_or_create(session, path, uid, dest).await
}

/// Move many messages from `path` to `dest` with as few IMAP commands as
/// possible: one SELECT, then a UID MOVE per chunk of the UID set (chunked so a
/// huge selection never overflows the server's command-length limit). Creates and
/// subscribes to `dest` on demand. Returns whether the destination was created.
pub(super) async fn move_messages(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    let mut created = false;
    let mut ensured_dest = false;
    for chunk in uids.chunks(300) {
        let set = uid_set(chunk);
        if uid_move(session, &set, dest).await.is_ok() {
            continue;
        }
        // First failure is most likely a missing destination mailbox — create,
        // subscribe, re-select the source, and retry this chunk (then the rest).
        if !ensured_dest {
            created = create_box(session, dest).await.is_ok();
            let _ = subscribe_box(session, dest).await;
            ensured_dest = true;
            sel(session, path).await?;
        }
        uid_move(session, &set, dest).await?;
    }
    Ok(created)
}

/// Create (and subscribe to) a new mailbox.
pub(super) async fn create_folder(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    create_box(session, path).await?;
    let _ = subscribe_box(session, path).await;
    Ok(())
}

/// RENAME a mailbox — the server carries any inferior hierarchy along
/// (RFC 3501 §6.3.5) — and follow the subscription to the new name.
pub(super) async fn rename_folder(
    session: &mut ImapSession,
    old_path: &str,
    new_path: &str,
) -> Result<(), async_imap::error::Error> {
    // Sub-folders go along with the RENAME, but their subscriptions are the
    // client's to move: Dovecot leaves them at the old names, and a client
    // that shows subscribed folders only (Thunderbird's default) would lose
    // them. So they are listed first, and each one's subscription is moved too.
    // async-imap sends the pattern as given: quoted here, names have spaces.
    let pattern = quote_mailbox(&format!("{old_path}*"));
    let children: Vec<String> = match session.list(Some(""), Some(&pattern)).await {
        Ok(stream) => stream
            .try_collect::<Vec<async_imap::types::Name>>()
            .await
            .unwrap_or_default()
            .iter()
            // Below it by the delimiter the server gives each name (#554).
            .filter_map(|n| {
                let d = imap_unquote(n.delimiter()?).into_owned();
                let path = imap_unquote(n.name()).into_owned();
                let below = path
                    .strip_prefix(old_path)?
                    .strip_prefix(d.as_str())
                    .is_some_and(|rest| !rest.is_empty());
                below.then_some(path)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    rename_box(session, old_path, new_path).await?;
    let _ = unsubscribe_box(session, old_path).await;
    let _ = subscribe_box(session, new_path).await;
    for child in children {
        let moved = format!("{new_path}{}", &child[old_path.len()..]);
        let _ = unsubscribe_box(session, &child).await;
        let _ = subscribe_box(session, &moved).await;
    }
    Ok(())
}

/// Delete a mailbox after moving all of its messages to `trash` (creating the
/// trash mailbox if needed). With no trash target the contents are discarded.
pub(super) async fn delete_folder(
    session: &mut ImapSession,
    path: &str,
    trash: Option<&str>,
) -> Result<(), async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    if mailbox.exists > 0 {
        if let Some(trash) = trash {
            if !trash.eq_ignore_ascii_case(path) {
                // Move everything to Trash; create it first if the move fails.
                if uid_move(session, "1:*", trash).await.is_err() {
                    let _ = create_box(session, trash).await;
                    let _ = subscribe_box(session, trash).await;
                    sel(session, path).await?;
                    uid_move(session, "1:*", trash).await?;
                }
            }
        }
    }
    // A mailbox can't be deleted while selected — close it first.
    let _ = session.close().await;
    delete_box(session, path).await?;
    Ok(())
}

/// Mark every message in a folder as read (`\Seen`) in one STORE.
pub(super) async fn mark_all_read(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    if mailbox.exists == 0 {
        return Ok(());
    }
    let _: Vec<Fetch> = store_uids(session, "1:*", "+FLAGS (\\Seen)")
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Mark a message as spam following common conventions: set the `$Junk` keyword
/// and clear `$NotJunk` (so server-side filters / other clients can learn), then
/// move it to the Junk folder. Keyword stores are best-effort — some servers
/// reject custom keywords — but the move is authoritative.
pub(super) async fn mark_spam(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    for query in ["+FLAGS ($Junk)", "-FLAGS ($NotJunk)"] {
        if let Ok(stream) = store_uids(session, uid.to_string(), query).await {
            let _: Result<Vec<Fetch>, _> = stream.try_collect().await;
        }
    }
    move_or_create(session, path, uid, dest).await
}

/// The reverse of [`mark_spam`] (#168): set `$NotJunk` and clear `$Junk` on
/// every uid (best-effort, as above), then move them back to `dest`.
pub(super) async fn mark_ham(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    let set = uid_set(uids);
    for query in ["+FLAGS ($NotJunk)", "-FLAGS ($Junk)"] {
        if let Ok(stream) = store_uids(session, set.clone(), query).await {
            let _: Result<Vec<Fetch>, _> = stream.try_collect().await;
        }
    }
    move_messages(session, path, uids, dest).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::borrow::Cow;

    /// A message moved in from another account keeps its read and starred
    /// state as IMAP flags, and asks for none when it had neither (#265).
    #[test]
    fn imported_mail_keeps_its_flags() {
        assert_eq!(
            import_flags(true, true).as_deref(),
            Some("(\\Seen \\Flagged)")
        );
        assert_eq!(import_flags(true, false).as_deref(), Some("(\\Seen)"));
        assert_eq!(import_flags(false, true).as_deref(), Some("(\\Flagged)"));
        assert_eq!(import_flags(false, false), None);
    }

    #[test]
    fn kept_flags_keep_the_ones_an_append_can_set() {
        let flags = vec![
            Flag::Seen,
            Flag::Deleted,
            Flag::Recent,
            Flag::Custom(Cow::Borrowed("$Label")),
            Flag::Custom(Cow::Borrowed("\\Weird")),
            Flag::Draft,
            Flag::Answered,
            Flag::Flagged,
            Flag::MayCreate,
        ];
        assert_eq!(
            kept_flags(flags.into_iter()),
            ["\\Seen", "$Label", "\\Draft", "\\Answered", "\\Flagged"]
        );
    }
}
