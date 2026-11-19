//! Messages on their way out (#434).
//!
//! Send hands a message here rather than straight to its worker. It is
//! written to the Outbox first (#678), hidden while this session has it, so
//! that nothing short of the server taking it can lose it. It waits out the
//! Undo Send delay with Undo on offer in a toast, then goes to the worker,
//! which answers with a `SendDone` carrying the same ticket. The Outbox shows
//! it only if it could not be sent.

use std::cell::RefCell;
use std::rc::Rc;

use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::worker::OutgoingMessage;

pub(super) struct Outgoing {
    pub(super) ticket: u64,
    pub(super) account_id: u32,
    /// The account as configured when Send was pressed: quitting builds the
    /// message from it, after the app's own state is gone.
    pub(super) account: AccountConfig,
    pub(super) message: Box<OutgoingMessage>,
    pub(super) sent_path: Option<String>,
    /// Where it was written, for Undo to reopen it there.
    pub(super) origin: Origin,
    /// Still waiting out the delay, so Undo can take it back. False once the
    /// delay is over.
    pub(super) held: bool,
    /// Its copy in the Outbox.
    pub(super) journal: Journal,
    /// The delay ended while the copy was still being written: it goes to
    /// its worker once written.
    pub(super) due: bool,
    pub(super) toast: adw::Toast,
}

/// A message's copy in the Outbox (#678).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Journal {
    /// Being written, on a thread of its own: building a message reads its
    /// attachments and may sign or encrypt it.
    Writing,
    /// Written, under this Outbox id.
    Kept(u32),
    /// Could not be written; the message is held in memory alone.
    NotKept,
}

/// The composer a message was sent from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Origin {
    /// The split reply beside the card it answers.
    Split((u32, u32)),
    /// The editor covering the reading pane.
    Pane,
    /// A window of its own.
    Window,
}

pub(super) type OutgoingList = Rc<RefCell<Vec<Outgoing>>>;

impl Outgoing {
    /// What the toast says while the message waits out its delay.
    pub(super) fn sending_title(&self) -> String {
        match self.message.subject.trim() {
            "" => i18n("Sending message…"),
            subject => i18n_f("Sending “{subject}”…", &[("subject", subject)]),
        }
    }
}

/// Quitting: a message whose copy is in the Outbox is safe there, and the
/// next launch sends it. One whose copy is still being written, or could not
/// be, goes in the Outbox now, due at once. The process exits right after,
/// so this is the last chance.
pub(super) fn park_held(list: &OutgoingList) {
    let Ok(list) = list.try_borrow() else { return };
    if list.iter().all(|o| matches!(o.journal, Journal::Kept(_))) {
        return;
    }
    let cache = match crate::cache::Cache::open() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not open the cache to keep unsent mail at quit: {e}");
            return;
        }
    };
    let now = crate::datefmt::now();
    for o in list
        .iter()
        .filter(|o| !matches!(o.journal, Journal::Kept(_)))
    {
        let kept = crate::worker::park_for_next_launch(
            &cache,
            o.account_id,
            &o.account,
            &o.message,
            o.sent_path.as_deref(),
            o.message.send_at.map_or(now, |at| at.max(now)),
        );
        tracing::info!(
            "quit before message {} was in the Outbox: kept there now: {kept}",
            o.ticket
        );
    }
}

/// The messages `account_id`'s worker had when it panicked (#684), kept in
/// the Outbox; their tickets, for the app to settle as failed sends.
pub(super) fn keep_stopped(
    list: &OutgoingList,
    cache: &crate::cache::Cache,
    account_id: u32,
) -> Vec<u64> {
    list.borrow()
        .iter()
        // One still being written has not reached the worker: it goes to
        // the new one when written.
        .filter(|o| o.account_id == account_id && !o.held && o.journal != Journal::Writing)
        .map(|o| {
            crate::worker::keep_after_stop(
                cache,
                o.account_id,
                &o.account,
                &o.message,
                o.sent_path.as_deref(),
            );
            o.ticket
        })
        .collect()
}
