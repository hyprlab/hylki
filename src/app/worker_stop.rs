//! An account's worker that panicked (#684). Its thread is gone with every
//! request it had not answered, a Send among them, which used to sit on
//! "Sending…" until quitting dropped it. The messages it had go to the
//! Outbox, and the worker is started again unless it keeps stopping.

use std::time::{Duration, Instant};

use relm4::prelude::*;

use crate::i18n::i18n_f;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;

/// More stops than this within [`STOP_WINDOW`] leave the worker stopped: a
/// panic on something it meets at every sync would only repeat.
const MAX_STOPS: usize = 3;
const STOP_WINDOW: Duration = Duration::from_secs(10 * 60);

impl AppModel {
    pub(super) fn worker_stopped(&mut self, account_id: u32, sender: &ComponentSender<Self>) {
        let mut tickets = Vec::new();
        match self.cache.as_ref() {
            Some(cache) => {
                tickets.extend(super::outgoing::keep_stopped(
                    &self.outgoing,
                    cache,
                    account_id,
                ));
                tickets.extend(super::merge_send::keep_stopped(
                    &self.merges,
                    cache,
                    account_id,
                ));
            }
            None => tracing::warn!("no cache to keep the mail of a stopped worker in"),
        }

        let stops = self.worker_stops.entry(account_id).or_default();
        stops.retain(|t| t.elapsed() < STOP_WINDOW);
        stops.push(Instant::now());
        let restart = stops.len() <= MAX_STOPS;
        let account = self.worker_account(account_id);
        let label = account
            .as_ref()
            .map(|a| a.display_label())
            .unwrap_or_default();
        // Its sync ended with it.
        self.busy.remove(&account_id);
        self.update_busy_indicator();
        match account.filter(|_| restart) {
            Some(account) => {
                let worker = Self::spawn_worker(account_id, Some(account), sender);
                self.workers.insert(account_id, worker);
                self.send_to(account_id, MailRequest::LoadOutbox);
                self.send_to(account_id, MailRequest::FlushOutbox { id: None });
            }
            // Nothing is listening: sending to it would only drop the request.
            None => {
                self.workers.remove(&account_id);
            }
        }
        let kept = !tickets.is_empty();
        // Settled after the restart, so a merge hands its next message to the
        // new worker.
        for ticket in tickets {
            sender.input(AppMsg::SendDone {
                ticket,
                sent: false,
            });
        }
        let text = match (restart, kept) {
            (true, true) => i18n_f(
                "The connection to {account} stopped after an internal error and was restarted. Mail that was being sent is in the Outbox.",
                &[("account", &label)],
            ),
            (true, false) => i18n_f(
                "The connection to {account} stopped after an internal error and was restarted.",
                &[("account", &label)],
            ),
            (false, true) => i18n_f(
                "The connection to {account} keeps stopping after an internal error. Mail that was being sent is in the Outbox. Restart Hylki to connect again.",
                &[("account", &label)],
            ),
            (false, false) => i18n_f(
                "The connection to {account} keeps stopping after an internal error. Restart Hylki to connect again.",
                &[("account", &label)],
            ),
        };
        sender.input(AppMsg::Error {
            account_id,
            text,
            connectivity: false,
        });
    }
}
