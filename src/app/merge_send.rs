//! Sending a Mail Merge (#447). The messages go to the worker one at a
//! time, each once the one before it is done and, with a rate set in the
//! dialog, once its wait is over: a provider that limits how fast mail goes
//! out is never sent more than that, and Stop has something left to stop.
//! A toast counts them off while they go.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use relm4::prelude::*;

use crate::config::AccountConfig;
use crate::i18n::{i18n, ni18n_f};
use crate::worker::{MailRequest, OutgoingMessage};

use super::model::AppModel;
use super::msg::AppMsg;

pub(super) struct MergeRun {
    id: u64,
    account_id: u32,
    /// The account as it was at Send: quitting parks what is left with it.
    account: AccountConfig,
    sent_path: Option<String>,
    waiting: VecDeque<OutgoingMessage>,
    total: u32,
    sent: u32,
    failed: u32,
    stopped: bool,
    /// The ticket of the message the worker has now.
    in_flight: Option<u64>,
    interval: Option<Duration>,
    /// When the last message went to the worker, to wait from.
    last: Option<Instant>,
    /// A wait is running; its end sends the next message.
    waiting_out: bool,
    toast: adw::Toast,
}

/// The merges under way, shared with the quit paths.
pub(super) type MergeRuns = Rc<RefCell<Vec<MergeRun>>>;

impl MergeRun {
    fn progress_title(&self) -> String {
        let done = self.sent + self.failed;
        if done == 0 {
            return ni18n_f(
                "Sending {n} message…",
                "Sending {n} messages…",
                self.total,
                &[("n", &self.total.to_string())],
            );
        }
        ni18n_f(
            "Sent {n} of {total} message",
            "Sent {n} of {total} messages",
            self.total,
            &[("n", &done.to_string()), ("total", &self.total.to_string())],
        )
    }

    fn result_title(&self) -> String {
        if self.sent == self.total {
            return ni18n_f(
                "Sent {n} message",
                "Sent {n} messages",
                self.total,
                &[("n", &self.total.to_string())],
            );
        }
        // Each one that failed has said why already, and waits in the
        // Outbox or came back in a composer.
        ni18n_f(
            "Sent {n} of {total} message",
            "Sent {n} of {total} messages",
            self.total,
            &[
                ("n", &self.sent.to_string()),
                ("total", &self.total.to_string()),
            ],
        )
    }

    fn finished(&self) -> bool {
        self.in_flight.is_none() && (self.stopped || self.waiting.is_empty())
    }
}

/// Quitting: what a merge has not handed to its worker yet goes in the
/// Outbox, each at the time the rate would have sent it, so the next launch
/// carries on rather than sending the rest at once.
pub(super) fn park_waiting(runs: &MergeRuns) {
    let Ok(runs) = runs.try_borrow() else { return };
    if runs.iter().all(|r| r.stopped || r.waiting.is_empty()) {
        return;
    }
    let cache = match crate::cache::Cache::open() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not open the cache to keep a mail merge at quit: {e}");
            return;
        }
    };
    let now = crate::datefmt::now();
    for run in runs.iter().filter(|r| !r.stopped) {
        let step = run.interval.map_or(0, |d| d.as_secs_f64().ceil() as i64);
        let mut kept = 0;
        for (i, message) in run.waiting.iter().enumerate() {
            let at = now + step * (i as i64 + 1);
            if crate::worker::park_for_next_launch(
                &cache,
                run.account_id,
                &run.account,
                message,
                run.sent_path.as_deref(),
                at,
            ) {
                kept += 1;
            }
        }
        tracing::info!(
            "quit during a mail merge: {kept} of {} messages kept in the Outbox",
            run.waiting.len()
        );
    }
}

impl AppModel {
    /// A merge from a composer: its addresses remembered, its toast up, and
    /// the first message on its way.
    pub(super) fn send_merged(
        &mut self,
        messages: Vec<OutgoingMessage>,
        interval: Option<Duration>,
        sender: &ComponentSender<Self>,
    ) {
        let Some(account_id) = messages.first().map(|m| m.from_account_id) else {
            return;
        };
        let recipients: Vec<(String, String)> = messages
            .iter()
            .flat_map(|m| [&m.to, &m.cc, &m.bcc])
            .flat_map(|f| crate::worker::parse_recipients(f))
            .collect();
        if let Some(c) = self.cache.as_ref() {
            c.record_addresses(&recipients);
        }
        let Some(account) = account_id
            .checked_sub(1)
            .and_then(|i| self.effective_config().get(i as usize).cloned())
        else {
            return;
        };
        self.send_ticket += 1;
        let id = self.send_ticket;
        let total = messages.len() as u32;
        tracing::info!(
            "mail merge: {total} messages from account {account_id}, one every {:?}",
            interval.unwrap_or_default()
        );
        let toast = adw::Toast::builder()
            .priority(adw::ToastPriority::High)
            .timeout(0)
            .button_label(i18n("Stop"))
            .build();
        let s = sender.clone();
        toast.connect_button_clicked(move |_| s.input(AppMsg::StopMerge(id)));
        let run = MergeRun {
            id,
            account_id,
            account,
            sent_path: self.sent_copy_path(account_id),
            waiting: messages.into(),
            total,
            sent: 0,
            failed: 0,
            stopped: false,
            in_flight: None,
            interval,
            last: None,
            waiting_out: false,
            toast: toast.clone(),
        };
        toast.set_title(&run.progress_title());
        self.merges.borrow_mut().push(run);
        self.show_toast(&toast);
        self.next_merged(id, sender);
    }

    /// The next message of merge `id` to its worker, if the one before it
    /// is done and the rate allows it now; otherwise a wait, whose end
    /// comes back here.
    pub(super) fn next_merged(&mut self, id: u64, sender: &ComponentSender<Self>) {
        let mut runs = self.merges.borrow_mut();
        let Some(run) = runs.iter_mut().find(|r| r.id == id) else {
            return;
        };
        if run.in_flight.is_some() || run.waiting_out || run.stopped || run.waiting.is_empty() {
            return;
        }
        let wait = match (run.interval, run.last) {
            (Some(d), Some(last)) => d.saturating_sub(last.elapsed()),
            _ => Duration::ZERO,
        };
        if !wait.is_zero() {
            run.waiting_out = true;
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(wait, move || s.input(AppMsg::MergeTick(id)));
            return;
        }
        if !self.workers.contains_key(&run.account_id) {
            // The account went while the merge was under way: nothing
            // would ever answer for these.
            run.failed += run.waiting.len() as u32;
            run.waiting.clear();
            drop(runs);
            self.end_merge(id);
            return;
        }
        let Some(message) = run.waiting.pop_front() else {
            return;
        };
        self.send_ticket += 1;
        let ticket = self.send_ticket;
        run.in_flight = Some(ticket);
        run.last = Some(Instant::now());
        let (account_id, sent_path) = (run.account_id, run.sent_path.clone());
        drop(runs);
        self.send_to(
            account_id,
            MailRequest::Send {
                message: Box::new(message),
                sent_path,
                ticket,
            },
        );
    }

    /// A merge's wait is over.
    pub(super) fn on_merge_tick(&mut self, id: u64, sender: &ComponentSender<Self>) {
        if let Some(run) = self.merges.borrow_mut().iter_mut().find(|r| r.id == id) {
            run.waiting_out = false;
        }
        self.next_merged(id, sender);
    }

    /// Stop on a merge's toast: nothing more goes. The message the worker
    /// has already is past stopping, and is counted when it is done.
    pub(super) fn stop_merge(&mut self, id: u64) {
        let finished = {
            let mut runs = self.merges.borrow_mut();
            let Some(run) = runs.iter_mut().find(|r| r.id == id) else {
                return;
            };
            run.stopped = true;
            run.finished()
        };
        if finished {
            self.end_merge(id);
        }
    }

    /// The worker is done with a message; true if it was a merge's.
    pub(super) fn merged_done(
        &mut self,
        ticket: u64,
        sent: bool,
        sender: &ComponentSender<Self>,
    ) -> bool {
        let (id, finished) = {
            let mut runs = self.merges.borrow_mut();
            let Some(run) = runs.iter_mut().find(|r| r.in_flight == Some(ticket)) else {
                return false;
            };
            run.in_flight = None;
            if sent {
                run.sent += 1;
            } else {
                run.failed += 1;
            }
            run.toast.set_title(&run.progress_title());
            (run.id, run.finished())
        };
        if finished {
            self.end_merge(id);
        } else {
            self.next_merged(id, sender);
        }
        true
    }

    /// A merge is over: its toast gives way to one with the count.
    fn end_merge(&mut self, id: u64) {
        let run = {
            let mut runs = self.merges.borrow_mut();
            let Some(i) = runs.iter().position(|r| r.id == id) else {
                return;
            };
            runs.remove(i)
        };
        tracing::info!(
            "mail merge: {} of {} sent, {} failed{}",
            run.sent,
            run.total,
            run.failed,
            if run.stopped { ", stopped" } else { "" }
        );
        run.toast.dismiss();
        self.show_toast(&adw::Toast::new(&run.result_title()));
    }
}
