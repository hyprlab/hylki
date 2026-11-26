//! A folder that has not changed since its last full load is refreshed with
//! its flags alone (#676). Every open of a folder fetched the newest window's
//! envelopes, structures and previews again and searched for every UID, which
//! on Yahoo held the connection for 6 to 7 seconds; a move or an Empty Trash
//! count waited behind each one.
//!
//! The same UIDVALIDITY, UIDNEXT and message count as the last full load
//! means nothing arrived (UIDNEXT would move) and nothing left (the count
//! would drop), so the server holds the same messages. The same set of UIDs
//! in the cache as that load left there means the index still matches them.
//! Only flags can differ then, and a `(UID FLAGS)` fetch of the window and
//! the folder-wide flag searches that follow it catch those.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex};

use async_imap::types::{Flag, Mailbox};

use crate::models::Message;
use crate::worker::imap::summary::apply_flags;
use crate::worker::imap::wire::logged_fetch;
use crate::worker::imap::ImapSession;

/// What a folder's SELECT said, and what its index held, after a full load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    validity: u32,
    uid_next: u32,
    exists: u32,
    /// A fetch made without previews must not stand in for one with them.
    previews: bool,
    uids: u64,
}

impl Stamp {
    /// `None` for a server that leaves out UIDVALIDITY or UIDNEXT: without
    /// them an unchanged folder cannot be told from a changed one.
    pub(super) fn new(mailbox: &Mailbox, previews: bool, messages: &[Message]) -> Option<Self> {
        Some(Self {
            validity: mailbox.uid_validity?,
            uid_next: mailbox.uid_next?,
            exists: mailbox.exists,
            previews,
            uids: uid_set_hash(messages),
        })
    }
}

static LOADED: LazyLock<Mutex<HashMap<(u32, String), Stamp>>> = LazyLock::new(Default::default);

/// Note a full load. A load that cannot be stamped forgets the last one.
pub(super) fn record(account_id: u32, path: &str, stamp: Option<Stamp>) {
    // A poisoned lock is skipped, as the worker's other session maps are.
    let Ok(mut loaded) = LOADED.lock() else {
        return;
    };
    let key = (account_id, path.to_string());
    match stamp {
        Some(s) => loaded.insert(key, s),
        None => loaded.remove(&key),
    };
}

/// Whether the folder and its index are as the last full load left them.
pub(super) fn unchanged(account_id: u32, path: &str, now: Option<Stamp>) -> bool {
    let Some(now) = now else { return false };
    LOADED
        .lock()
        .ok()
        .and_then(|loaded| loaded.get(&(account_id, path.to_string())).copied())
        == Some(now)
}

fn uid_set_hash(messages: &[Message]) -> u64 {
    let mut uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
    uids.sort_unstable();
    let mut h = DefaultHasher::new();
    uids.hash(&mut h);
    h.finish()
}

/// The newest `count` of `cached`, newest first, with the server's flags.
/// `None` when the server lists a message the index does not hold, which
/// only a full fetch can fill in.
pub(super) async fn window_flags(
    session: &mut ImapSession,
    cached: &[Message],
    total: u32,
    count: u32,
) -> Result<Option<Vec<Message>>, async_imap::error::Error> {
    let start = total.saturating_sub(count - 1).max(1);
    let fetches = logged_fetch(session, &format!("{start}:{total}"), "(UID FLAGS)").await?;
    let flags: Vec<(u32, Vec<Flag<'_>>)> = fetches
        .iter()
        .filter_map(|f| Some((f.uid?, f.flags().collect())))
        .collect();
    Ok(refreshed(cached, &flags))
}

fn refreshed(cached: &[Message], flags: &[(u32, Vec<Flag<'_>>)]) -> Option<Vec<Message>> {
    let by_uid: HashMap<u32, &Message> = cached.iter().map(|m| (m.uid, m)).collect();
    let mut window = Vec::with_capacity(flags.len());
    for (uid, flags) in flags {
        let mut m = (*by_uid.get(uid)?).clone();
        apply_flags(&mut m, flags);
        window.push(m);
    }
    window.sort_by_key(|m| std::cmp::Reverse(m.uid));
    Some(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::test_support::flagged_message;

    fn mailbox(validity: u32, uid_next: u32, exists: u32) -> Mailbox {
        Mailbox {
            uid_validity: Some(validity),
            uid_next: Some(uid_next),
            exists,
            ..Mailbox::default()
        }
    }

    fn messages(uids: &[u32]) -> Vec<Message> {
        uids.iter()
            .map(|&uid| Message {
                subject: format!("subject {uid}"),
                ..flagged_message(uid)
            })
            .collect()
    }

    #[test]
    fn only_the_same_folder_and_index_count_as_unchanged() {
        let path = "INBOX-676-test";
        let held = messages(&[1, 2, 3]);
        let stamp = |mb: &Mailbox, held: &[Message]| Stamp::new(mb, true, held);
        assert!(
            !unchanged(99, path, stamp(&mailbox(7, 4, 3), &held)),
            "never loaded"
        );
        record(99, path, stamp(&mailbox(7, 4, 3), &held));
        assert!(unchanged(99, path, stamp(&mailbox(7, 4, 3), &held)));
        // In the order the cache hands them back, not the load's.
        let reversed: Vec<Message> = held.iter().rev().cloned().collect();
        assert!(unchanged(99, path, stamp(&mailbox(7, 4, 3), &reversed)));
        assert!(
            !unchanged(99, path, stamp(&mailbox(7, 5, 4), &held)),
            "new mail"
        );
        assert!(
            !unchanged(99, path, stamp(&mailbox(7, 4, 2), &held)),
            "mail gone"
        );
        assert!(
            !unchanged(99, path, stamp(&mailbox(8, 4, 3), &held)),
            "new UIDVALIDITY"
        );
        assert!(
            !unchanged(98, path, stamp(&mailbox(7, 4, 3), &held)),
            "another account"
        );
        let moved_out = messages(&[1, 3]);
        assert!(
            !unchanged(99, path, stamp(&mailbox(7, 4, 3), &moved_out)),
            "index changed"
        );
        assert!(
            !unchanged(99, path, Stamp::new(&mailbox(7, 4, 3), false, &held)),
            "no previews"
        );
        let no_next = Mailbox {
            uid_next: None,
            ..mailbox(7, 4, 3)
        };
        assert!(!unchanged(99, path, Stamp::new(&no_next, true, &held)));
        record(99, path, None);
        assert!(
            !unchanged(99, path, stamp(&mailbox(7, 4, 3), &held)),
            "forgotten"
        );
    }

    #[test]
    fn the_window_takes_the_servers_flags_over_the_cached_rows() {
        let cached = messages(&[1, 2, 3]);
        let flags = vec![(2, vec![]), (3, vec![Flag::Seen, Flag::Answered])];
        let window = refreshed(&cached, &flags).expect("all held");
        let uids: Vec<u32> = window.iter().map(|m| m.uid).collect();
        assert_eq!(uids, [3, 2], "newest first");
        assert!(!window[0].unread && window[0].answered && !window[0].starred);
        assert!(window[1].unread && !window[1].starred);
        assert_eq!(
            window[0].subject, cached[2].subject,
            "the rest is the cache's"
        );
    }

    #[test]
    fn a_message_the_index_lacks_needs_a_full_fetch() {
        let cached = messages(&[1, 2]);
        assert!(refreshed(&cached, &[(3, vec![])]).is_none());
    }
}
