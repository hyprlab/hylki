//! Forwarding mail from a filter: the same message the manual Forward would
//! write, built without a composer once the original's body and attachments
//! are in hand, and recorded in the [`config::ForwardedLog`] once it is on its
//! way. The rest of a rule's actions wait for that record (see
//! `AppModel::filter_pass`).

use crate::config;
use crate::i18n::i18n_f;
use crate::mail::quote::quoted_original;
use crate::models::Message;
use crate::worker::{MailRequest, OutgoingMessage};

use super::conversation::{body_key, BodyKey};
use super::filter_actions::ForwardJob;
use super::model::AppModel;
use super::prefill::{forward_header, forward_subject, stage_attachments};

/// How long a forward may wait for its message's body and files before the
/// filters give up on that attempt and let the next pass start it again.
const AUTO_FORWARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// A forward handed to a worker, until the worker says how it went.
pub(super) struct SentForward {
    key: String,
    rule: String,
    /// The message forwarded: its account, folder (to run the rest of the
    /// rule there once the forward is recorded) and date (for the log).
    account_id: u32,
    folder_id: u32,
    date: i64,
    /// What the worker hands back if the server refuses it (`Unsent`): the
    /// destinations, subject and body, to tell it from a message of the
    /// user's, who would not write the same text.
    to: String,
    subject: String,
    html: String,
    /// The server refused it: it is not queued anywhere, so it was not
    /// forwarded and must be tried again.
    refused: bool,
}

/// A forward waiting for the body or the attachments of its message.
pub(super) struct AutoForward {
    message: Message,
    job: ForwardJob,
    started: std::time::Instant,
}

/// The message a filter sends to forward `m` to `to`: the manual Forward's
/// subject and quoted original, with `attachments` (files on disk) attached.
/// No signature is added and the original's Message-ID goes in `forwards`, so
/// it is marked as forwarded once this has gone.
pub(super) fn forward_outgoing(
    m: &Message,
    to: &[String],
    attachments: Vec<String>,
) -> OutgoingMessage {
    let html =
        crate::mail::markdown::sanitize_outgoing(&quoted_original(&forward_header(m), &m.body));
    let body = crate::mail::markdown::html_to_text(&html);
    OutgoingMessage {
        from_account_id: m.account_id,
        from_alias: None,
        to: to.join(", "),
        cc: String::new(),
        bcc: String::new(),
        reply_to: String::new(),
        subject: forward_subject(&m.subject),
        body,
        html,
        attachments,
        in_reply_to: String::new(),
        references: String::new(),
        forwards: m.message_id.clone(),
        draft_origin: None,
        outbox_origin: None,
        sign: false,
        encrypt: false,
        send_at: None,
        calendar: None,
        request_receipt: false,
        receipt_report: None,
    }
}

impl AppModel {
    /// Drop forwards that have waited too long for their message, so a body
    /// that never arrives does not keep the key in flight for good.
    pub(super) fn purge_stale_auto_forwards(&mut self) {
        let stale: Vec<String> = self
            .auto_forwards
            .iter()
            .filter(|f| f.started.elapsed() > AUTO_FORWARD_TIMEOUT)
            .map(|f| f.job.key.clone())
            .collect();
        if stale.is_empty() {
            return;
        }
        tracing::warn!(
            "filter: {} forward(s) gave up waiting for their message",
            stale.len()
        );
        self.auto_forwards.retain(|f| !stale.contains(&f.job.key));
        for key in stale {
            self.auto_forward_inflight.remove(&key);
        }
    }

    /// Start forwarding `m` as `job` says. The message is fetched in full
    /// first when the list only carries its preview.
    pub(super) fn start_auto_forward(&mut self, m: Message, job: ForwardJob) {
        if !self.auto_forward_inflight.insert(job.key.clone()) {
            return;
        }
        tracing::info!(
            "filter: forwarding {}:{}:{} [{}]",
            m.account_id,
            m.folder_id,
            m.uid,
            job.rule
        );
        let target = body_key(&m);
        // A request for this message's body or files is already out when
        // another forward of it is waiting; its answer wakes both.
        let busy = self
            .auto_forwards
            .iter()
            .any(|f| body_key(&f.message) == target);
        self.auto_forwards.push(AutoForward {
            message: m,
            job,
            started: std::time::Instant::now(),
        });
        if !busy {
            self.advance_auto_forwards(target);
        }
    }

    /// A body, the files, or the sender check of message `id` arrived (or
    /// the files turned out not to exist, `no_attachments`): carry its
    /// waiting forwards on.
    pub(super) fn resume_auto_forwards(
        &mut self,
        account_id: u32,
        id: u32,
        key: Option<BodyKey>,
        no_attachments: bool,
    ) {
        // Without a key the event names a folder the app does not know, so
        // what it brought was cached nowhere and asking again would bring
        // the same. Those forwards stop here; their keys stay in flight,
        // which holds the message for the session.
        let Some(target) = key else {
            let before = self.auto_forwards.len();
            self.auto_forwards
                .retain(|f| !(f.message.account_id == account_id && f.message.id == id));
            if self.auto_forwards.len() != before {
                tracing::warn!("filter: {account_id}:{id} arrived from an unknown folder");
            }
            return;
        };
        if !self
            .auto_forwards
            .iter()
            .any(|f| body_key(&f.message) == target)
        {
            return;
        }
        if no_attachments {
            for f in self
                .auto_forwards
                .iter_mut()
                .filter(|f| body_key(&f.message) == target)
            {
                f.message.has_attachment = false;
            }
        }
        self.advance_auto_forwards(target);
    }

    /// Drop the waiting forwards of `target` with a warning. Their keys
    /// stay in flight, so the message is held for the session.
    fn abandon_auto_forwards(&mut self, target: BodyKey, why: &str) {
        tracing::warn!("filter: {target:?} not forwarded: {why}");
        self.auto_forwards
            .retain(|f| body_key(&f.message) != target);
    }

    /// Send the forwards of message `target` if its body and files are in
    /// hand; otherwise ask for what is missing and wait for the answer.
    fn advance_auto_forwards(&mut self, target: BodyKey) {
        let Some(first) = self
            .auto_forwards
            .iter()
            .find(|f| body_key(&f.message) == target)
        else {
            return;
        };
        let m = self.with_cached_body(first.message.clone());
        if self.fetch_for_copy(&m) {
            return;
        }
        // Nothing to fetch it from and nothing in hand, or a body the
        // server would not hand over: sending would forward an empty
        // message.
        if m.body.is_empty() || m.body == "(empty message)" {
            self.abandon_auto_forwards(target, "no body");
            return;
        }
        // The list's preview names most encrypted mail, but not when there
        // is none (previews off, or refused by the server). The verdict
        // that follows the body then decides, and the forward waits for it.
        // A cached body never needs it: encrypted mail is not cached.
        match self.sender_cache.get(&target) {
            Some(check) if check.pgp.as_ref().is_some_and(|p| p.encrypted) => {
                self.abandon_auto_forwards(target, "encrypted");
                return;
            }
            None if m.preview.trim().is_empty() => return,
            _ => {}
        }
        let (account_id, folder_id, uid) = target;
        let files: Vec<String> = self
            .attachment_cache
            .get(&target)
            .map(|items| {
                stage_attachments(
                    &format!("hylki-autofwd-{account_id}-{folder_id}-{uid}"),
                    items,
                )
            })
            .unwrap_or_default()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let (sending, waiting): (Vec<AutoForward>, Vec<AutoForward>) =
            std::mem::take(&mut self.auto_forwards)
                .into_iter()
                .partition(|f| body_key(&f.message) == target);
        self.auto_forwards = waiting;
        for f in sending {
            self.send_ticket += 1;
            let ticket = self.send_ticket;
            let out = forward_outgoing(&m, &f.job.to, files.clone());
            self.auto_forward_tickets.insert(
                ticket,
                SentForward {
                    key: f.job.key.clone(),
                    rule: f.job.rule.clone(),
                    account_id,
                    folder_id,
                    date: m.timestamp,
                    to: out.to.clone(),
                    subject: out.subject.clone(),
                    html: out.html.clone(),
                    refused: false,
                },
            );
            self.send_to(
                account_id,
                MailRequest::Send {
                    message: Box::new(out),
                    sent_path: None,
                    ticket,
                },
            );
        }
    }

    /// The worker handed back a message nothing could keep (`Unsent`). When
    /// it is a filter's forward the server refused, it is not forwarded: say
    /// so, and keep it out of the composer. True when it was one.
    pub(super) fn take_refused_auto_forward(
        &mut self,
        account_id: u32,
        message: &OutgoingMessage,
    ) -> bool {
        let Some(f) = self.auto_forward_tickets.values_mut().find(|f| {
            !f.refused
                && f.account_id == account_id
                && f.to == message.to
                && f.subject == message.subject
                && f.html == message.html
        }) else {
            return false;
        };
        f.refused = true;
        let rule = f.rule.clone();
        self.notice_error(i18n_f(
            "The forward of filter {rule} was refused. The message was left where it is and will be tried again",
            &[("rule", &rule)],
        ));
        true
    }

    /// A filter's forward was handed to the worker: `sent`, or queued in
    /// the Outbox, which retries it on its own. Either way it is recorded,
    /// so it is never sent a second time. A sent one lets the rest of the
    /// rule follow, so the filters run again over its folder; a queued one
    /// holds the message until the Outbox has sent it (see
    /// `filter_actions::plan_actions`). One the server refused is not
    /// recorded: the message stays held and the next pass tries again.
    pub(super) fn finish_auto_forward(&mut self, ticket: u64, sent: bool) {
        let Some(f) = self.auto_forward_tickets.remove(&ticket) else {
            return;
        };
        self.auto_forward_inflight.remove(&f.key);
        if f.refused {
            return;
        }
        let state = if sent {
            config::Forwarded::Sent
        } else {
            config::Forwarded::Queued
        };
        self.forwarded_log.record(f.key, f.date, state);
        if !sent {
            self.notice_error(i18n_f(
                "The forward of filter {rule} is waiting in the Outbox",
                &[("rule", &f.rule)],
            ));
            return;
        }
        self.rerun_filters_on(f.account_id, f.folder_id);
    }

    /// Run the filters over a folder again, by reloading it: the rest of a
    /// rule's actions on a message whose forward has just gone. A folder
    /// other than the Inbox is filtered on this one load, as it was by the
    /// manual run that started the forward.
    fn rerun_filters_on(&mut self, account_id: u32, folder_id: u32) {
        let Some(path) = self.folder_path(account_id, folder_id) else {
            return;
        };
        if self.inbox_of(account_id).map(|f| f.id) != Some(folder_id) {
            self.filter_followups.insert((account_id, folder_id));
        }
        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    #[test]
    fn subject_gets_fwd_prefix_once() {
        let mut m = msg(1, 1, 1);
        m.subject = "Invoice 12".into();
        let out = forward_outgoing(&m, &["a@x.org".into()], Vec::new());
        assert_eq!(out.subject, "Fwd: Invoice 12");
        m.subject = "Fwd: Invoice 12".into();
        assert_eq!(
            forward_outgoing(&m, &["a@x.org".into()], Vec::new()).subject,
            "Fwd: Invoice 12"
        );
    }

    #[test]
    fn quotes_the_original_and_sets_forwards() {
        let mut m = msg(1, 1, 1);
        m.message_id = "orig@x".into();
        m.body = "Please pay by Friday".into();
        let out = forward_outgoing(&m, &["a@x.org".into()], vec!["/tmp/f.pdf".into()]);
        assert_eq!(out.forwards, "orig@x");
        assert!(out.html.contains("Forwarded message"));
        assert!(out.body.contains("Please pay by Friday"));
        assert_eq!(out.attachments, vec!["/tmp/f.pdf".to_string()]);
        assert!(!out.sign && !out.encrypt && out.send_at.is_none());
    }

    #[test]
    fn recipients_are_joined() {
        let m = msg(1, 1, 1);
        let out = forward_outgoing(&m, &["a@x.org".into(), "b@y.org".into()], Vec::new());
        assert_eq!(out.to, "a@x.org, b@y.org");
    }
}
