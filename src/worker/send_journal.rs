//! The Outbox as the journal of every send (#678).
//!
//! A message is written to the Outbox when Send is pressed, before its
//! worker has it, and leaves only once the server has taken it. Whatever
//! happens in between (a failure, a quit, a crash, a worker that panics, a
//! power cut) it is still there, and the next flush sends it. Before, a
//! message lived in memory until it went: one that waited in its worker's
//! queue behind two minutes of downloads was lost when Hylki was closed.
//!
//! While this session's worker has the message it is "in hand": hidden from
//! the Outbox list, so the Outbox does not show for every send, and from the
//! Outbox's flush, which would send it a second time. A message in hand is
//! hidden by this process alone, so after a crash it is an ordinary queued
//! message again.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard};

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::i18n;
use crate::models::OutboxItem;
use crate::worker::compose::build::restore_msgid_case;
use crate::worker::outbox::build_for_outbox;
use crate::worker::OutgoingMessage;

static IN_HAND: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

fn in_hand_set() -> MutexGuard<'static, Option<HashSet<u32>>> {
    IN_HAND.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write `msg` to the Outbox, in hand, and return its Outbox id, which goes
/// with it to its worker as its `outbox_origin`. The version it was edited
/// from, if any, is dropped once this one is written. Blocking: it builds
/// the message, which reads its attachments and may sign or encrypt it.
pub(crate) fn write(
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
) -> Option<u32> {
    let cache = Cache::open()
        .map_err(|e| tracing::warn!("could not open the cache to journal a send: {e}"))
        .ok()?;
    write_to(&cache, account_id, account, msg, sent_path)
}

/// [`write`] into `cache`.
pub(super) fn write_to(
    cache: &Cache,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
) -> Option<u32> {
    let mut msg = msg.clone();
    restore_msgid_case(Some(cache), &mut msg);
    let built = build_for_outbox(account, &msg)?;
    // What a crash leaves in the Outbox says why it is there; a failure
    // that is reported replaces it.
    let note = i18n("Hylki closed before the message was sent.");
    let id = {
        // Held while the row is written, so a flush that reads the Outbox
        // meanwhile cannot see the row before it is in hand.
        let mut guard = in_hand_set();
        let id = built.insert(cache, account_id, &msg, sent_path, &note, msg.send_at)?;
        guard.get_or_insert_with(HashSet::new).insert(id);
        id
    };
    if let Some(old) = msg.outbox_origin {
        cache.delete_outbox(old);
    }
    crate::desktop::send_log::note_message(&account.email, &msg, "written to the Outbox");
    Some(id)
}

/// Whether the Outbox row `id` is a message this session's worker has.
pub(crate) fn in_hand(id: u32) -> bool {
    in_hand_set().as_ref().is_some_and(|s| s.contains(&id))
}

/// The worker is done with `id`: sent and deleted, or kept in the Outbox
/// for the user to see.
pub(crate) fn release(id: u32) {
    if let Some(set) = in_hand_set().as_mut() {
        set.remove(&id);
    }
}

/// Leave out of `items` the messages in hand. Called after the Outbox was
/// read, so a row written meanwhile is either not in `items` or in the set.
pub(super) fn hide_in_hand(items: &mut Vec<OutboxItem>) {
    if let Some(set) = in_hand_set().as_ref() {
        items.retain(|i| !set.contains(&i.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::outbox::{drop_superseded_outbox, due_outbox_items, send_failed};
    use crate::worker::test_support::{sample_account, sample_outgoing};
    use crate::worker::WorkerEvent;

    /// Every test cache numbers its Outbox from 1, and what is in hand is
    /// the process's: these tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn turn() -> MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn journaled(cache: &Cache) -> (OutgoingMessage, u32) {
        let mut msg = sample_outgoing();
        msg.to = "ann@example.com".into();
        let id = write_to(cache, 1, &sample_account(), &msg, Some("Sent")).expect("written");
        msg.outbox_origin = Some(id);
        (msg, id)
    }

    fn listed(cache: &Cache) -> Vec<crate::models::OutboxItem> {
        let events = std::cell::RefCell::new(Vec::new());
        crate::worker::outbox::emit_outbox(Some(cache), 1, &|e| events.borrow_mut().push(e));
        match events.into_inner().pop() {
            Some(WorkerEvent::Outbox { items }) => items,
            other => panic!("{other:?}"),
        }
    }

    /// #678: a message is in the Outbox from the moment Send is pressed,
    /// but neither listed nor flushed while its worker has it.
    #[test]
    fn a_message_on_its_way_is_kept_but_hidden() {
        let _turn = turn();
        let cache = Cache::in_memory().expect("cache");
        let (_, id) = journaled(&cache);
        assert_eq!(cache.outbox_items(1).len(), 1);
        assert!(in_hand(id));
        assert!(listed(&cache).is_empty());
        assert!(due_outbox_items(&cache, 1, None).is_empty());
        assert!(due_outbox_items(&cache, 1, Some(id)).is_empty());
        // What a crash leaves: an ordinary queued message.
        release(id);
        assert_eq!(listed(&cache).len(), 1);
        assert_eq!(due_outbox_items(&cache, 1, None).len(), 1);
    }

    #[test]
    fn a_sent_message_leaves_the_outbox() {
        let _turn = turn();
        let cache = Cache::in_memory().expect("cache");
        let (msg, id) = journaled(&cache);
        drop_superseded_outbox(Some(&cache), 1, &msg, &|_| {});
        assert!(cache.outbox_items(1).is_empty());
        assert!(!in_hand(id));
    }

    /// A failure keeps the very copy written at Send, with the reason, and
    /// shows it: one row, not a second one built again.
    #[test]
    fn a_failed_send_shows_the_copy_written_at_send() {
        let _turn = turn();
        let cache = Cache::in_memory().expect("cache");
        let (msg, id) = journaled(&cache);
        send_failed(
            Some(&cache),
            1,
            &sample_account(),
            &msg,
            Some("Sent"),
            "connection reset",
            false,
            &|_| {},
        );
        let items = listed(&cache);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id);
        assert_eq!(items[0].last_error, "connection reset");
        assert!(!in_hand(id));
    }

    /// A refused message used to go back to the composer alone: in the
    /// Outbox already, it stays there, and nothing is reopened.
    #[test]
    fn a_refused_send_stays_in_the_outbox() {
        let _turn = turn();
        let cache = Cache::in_memory().expect("cache");
        let (msg, id) = journaled(&cache);
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &sample_account(),
            &msg,
            None,
            "550 no such user",
            true,
            &|e| events.borrow_mut().push(e),
        );
        assert!(!events
            .into_inner()
            .iter()
            .any(|e| matches!(e, WorkerEvent::Unsent(_))));
        assert_eq!(listed(&cache).len(), 1);
        assert!(!in_hand(id));
    }

    /// Sent from the Outbox after an edit: the version it was edited from
    /// goes once the new one is written.
    #[test]
    fn writing_replaces_the_version_it_was_edited_from() {
        let _turn = turn();
        let cache = Cache::in_memory().expect("cache");
        let (mut msg, old) = journaled(&cache);
        release(old);
        msg.subject = "Edited".into();
        let id = write_to(&cache, 1, &sample_account(), &msg, None).expect("written");
        let items = cache.outbox_items(1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id);
        assert_eq!(items[0].subject, "Edited");
        release(id);
    }
}
