//! Messages on their way out (#434).
//!
//! Send hands a message here rather than straight to its worker. It waits out
//! the Undo Send delay with Undo on offer in a toast, then goes to the worker,
//! which answers with a `SendDone` carrying the same ticket. Until then the
//! Outbox lists it, so a message is never out of sight between the composer
//! closing and its copy landing in Sent.

use std::cell::RefCell;
use std::rc::Rc;

use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::OutboxItem;
use crate::worker::OutgoingMessage;

pub(super) struct Outgoing {
    pub(super) ticket: u64,
    pub(super) account_id: u32,
    /// The account as configured when Send was pressed: quitting builds the
    /// message from it, after the app's own state is gone.
    pub(super) account: AccountConfig,
    pub(super) message: Box<OutgoingMessage>,
    pub(super) sent_path: Option<String>,
    pub(super) queued_at: i64,
    /// Where it was written, for Undo to reopen it there.
    pub(super) origin: Origin,
    /// Still waiting out the delay, so Undo can take it back. False once the
    /// worker has it.
    pub(super) held: bool,
    pub(super) toast: adw::Toast,
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

/// The Outbox row id of a message on its way out. Counted down from the top
/// of the range, where the database's own ids for queued messages never reach.
pub(super) fn row_id(ticket: u64) -> u32 {
    u32::MAX - ticket as u32
}

impl Outgoing {
    /// What the toast says while the message waits out its delay.
    pub(super) fn sending_title(&self) -> String {
        match self.message.subject.trim() {
            "" => i18n("Sending message…"),
            subject => i18n_f("Sending “{subject}”…", &[("subject", subject)]),
        }
    }

    /// Its row in the Outbox.
    pub(super) fn as_outbox_item(&self) -> OutboxItem {
        let m = &self.message;
        let recipients = [&m.to, &m.cc]
            .iter()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let from_addr = m
            .from_alias
            .as_deref()
            .map(|f| crate::config::split_identity(f).1)
            .unwrap_or_else(|| self.account.email.clone());
        OutboxItem {
            id: row_id(self.ticket),
            account_id: self.account_id,
            from_addr,
            rcpts: Vec::new(),
            recipients,
            subject: m.subject.clone(),
            preview: m
                .body
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(160)
                .collect(),
            raw: Vec::new(),
            sent_path: self.sent_path.clone(),
            queued_at: self.queued_at,
            attempts: 0,
            last_error: String::new(),
            send_at: None,
            forwards: m.forwards.clone(),
            status: if self.held {
                i18n("Waiting to send")
            } else {
                i18n("Sending…")
            },
        }
    }
}

/// Quitting: every message still waiting out its delay goes in the Outbox,
/// due now, so the next launch sends it. The process exits right after, so
/// this is the last chance. One already with its worker is past saving here,
/// as it always was.
pub(super) fn park_held(list: &OutgoingList) {
    let Ok(list) = list.try_borrow() else { return };
    if !list.iter().any(|o| o.held) {
        return;
    }
    let cache = match crate::cache::Cache::open() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not open the cache to keep unsent mail at quit: {e}");
            return;
        }
    };
    for o in list.iter().filter(|o| o.held) {
        let kept = crate::worker::park_for_next_launch(
            &cache,
            o.account_id,
            &o.account,
            &o.message,
            o.sent_path.as_deref(),
            crate::datefmt::now(),
        );
        tracing::info!(
            "quit during Undo Send: message {} kept in the Outbox: {kept}",
            o.ticket
        );
    }
}
