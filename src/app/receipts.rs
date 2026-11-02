//! Read receipts (#520): which requests a reader asks about, sending the
//! receipt, and remembering the answer.

use relm4::prelude::*;

use crate::config::{self, ReceiptPolicy};
use crate::i18n::{i18n, i18n_f};
use crate::mail::receipt::ReceiptRequest;
use crate::models::{FolderKind, Message, SenderCheck};
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;

use super::conversation::{body_key, BodyKey};
use super::model::AppModel;
use super::send::machine_message;

impl AppModel {
    /// A message the app holds, by its body key: the one on screen, a
    /// member of the conversation on screen, or one in a loaded folder.
    pub(super) fn held_message(&self, key: BodyKey) -> Option<Message> {
        let hit = |m: &&Message| body_key(m) == key;
        self.current
            .iter()
            .chain(self.current_thread.iter())
            .find(hit)
            .or_else(|| {
                self.message_cache
                    .get(&(key.0, key.1))
                    .and_then(|ms| ms.iter().find(hit))
            })
            .or_else(|| {
                self.unified_slices
                    .get(&(key.0, key.1))
                    .and_then(|ms| ms.iter().find(hit))
            })
            .cloned()
    }

    /// Whether a receipt was sent for this message, or the request turned
    /// down: here, or by another mail app that marked it `$MDNSent`.
    fn receipt_answered(&self, m: &Message) -> bool {
        m.has_keyword("$MDNSent")
            || self
                .receipts_answered
                .iter()
                .any(|a| a.key.eq_ignore_ascii_case(&m.message_id))
    }

    /// Whether a request is the user's to answer: one they received, not
    /// answered yet, in a folder where mail is read rather than written or
    /// thrown away. Junk is left out on purpose: a receipt tells a spammer
    /// the address is read.
    fn receipt_open(&self, m: &Message) -> bool {
        if self.receipts == ReceiptPolicy::Never
            || m.message_id.is_empty()
            || self.receipt_answered(m)
            || self.outbox_item(m.account_id, m.id).is_some()
        {
            return false;
        }
        if matches!(
            self.folder_kind(m.account_id, m.folder_id),
            Some(
                FolderKind::Sent
                    | FolderKind::Drafts
                    | FolderKind::Templates
                    | FolderKind::Junk
                    | FolderKind::Trash
            )
        ) {
            return false;
        }
        let own = self.identities_map();
        let mine = own.get(&m.account_id);
        !mine.is_some_and(|a| a.iter().any(|a| a.eq_ignore_ascii_case(&m.from_addr)))
    }

    /// Whether Always may send this receipt without asking. RFC 8098 (2.1)
    /// says to ask when the receipt would go somewhere other than where the
    /// mail came from, and when the message was not addressed to the user
    /// (a list, a Bcc).
    fn receipt_automatic(&self, m: &Message, request: &ReceiptRequest) -> bool {
        if self.receipts != ReceiptPolicy::Always || !request.matches_return_path {
            return false;
        }
        let own = self.identities_map();
        let Some(mine) = own.get(&m.account_id) else {
            return false;
        };
        [&m.to, &m.cc]
            .into_iter()
            .flat_map(|f| crate::worker::parse_recipients(f))
            .any(|(_, addr)| mine.iter().any(|a| a.eq_ignore_ascii_case(&addr)))
    }

    /// A sender check as a reader is to show it: its verdict only if the
    /// account's provider made it, and the receipt request left in only when
    /// the card should ask about it. With `shown`, the card is on screen now,
    /// and a receipt Always may send goes at once instead.
    pub(super) fn reader_check(
        &mut self,
        m: &Message,
        check: &SenderCheck,
        shown: bool,
    ) -> Box<SenderCheck> {
        let mut out = Box::new(check.clone());
        self.vouchers.gate(Some(m.account_id), &mut out);
        let Some(request) = out.receipt.take() else {
            return out;
        };
        if !self.receipt_open(m) {
            return out;
        }
        if self.receipt_automatic(m, &request) {
            if shown {
                self.send_receipt(m, &request, true);
            }
            return out;
        }
        out.receipt = Some(request);
        out
    }

    /// [`Self::reader_check`] for a message known only by its body key.
    pub(super) fn reader_check_by_key(
        &mut self,
        key: BodyKey,
        check: &SenderCheck,
    ) -> Box<SenderCheck> {
        match self.held_message(key) {
            Some(m) => self.reader_check(&m, check, true),
            None => {
                let mut out = Box::new(SenderCheck {
                    receipt: None,
                    ..check.clone()
                });
                self.vouchers.gate(Some(key.0), &mut out);
                out
            }
        }
    }

    /// The banner's Send Receipt or Ignore.
    pub(super) fn answer_receipt(&mut self, m: &Message, send: bool) {
        let Some(request) = self
            .sender_cache
            .get(&body_key(m))
            .and_then(|c| c.receipt.clone())
        else {
            return;
        };
        if !self.receipt_open(m) {
            self.refresh_receipt_banners();
            return;
        }
        if send {
            self.send_receipt(m, &request, false);
        } else {
            self.record_receipt(m, false);
        }
    }

    /// Mail the receipt from the address the message was sent to, beside
    /// the conversation it answers. Like an invitation's answer, no copy is
    /// filed in Sent.
    fn send_receipt(&mut self, m: &Message, request: &ReceiptRequest, automatic: bool) {
        let (from_alias, from) = self.unsubscribe_from(m);
        let subject = if m.subject.trim().is_empty() {
            i18n("Read receipt")
        } else {
            i18n_f("Read: {subject}", &[("subject", &m.subject)])
        };
        let body = format!(
            "{}\n\n{}\n",
            i18n_f(
                "This is a read receipt for the message sent to {to} on {date} with the subject \"{subject}\".",
                &[
                    ("to", &from),
                    ("date", &crate::datefmt::date_time(m.timestamp)),
                    ("subject", &m.subject),
                ],
            ),
            i18n("It means the message was shown on the recipient's screen, not that it was read or understood."),
        );
        let mut out = machine_message(
            self.sending_account(m),
            from_alias,
            request.to.clone(),
            subject,
            body,
            m.message_id.clone(),
        );
        out.receipt_report = Some(crate::mail::receipt::report_fields(
            &from,
            request,
            &m.message_id,
            automatic,
        ));
        tracing::info!(
            "read receipt for <{}> sent to {} ({})",
            m.message_id,
            request.to,
            if automatic { "automatic" } else { "by hand" }
        );
        self.send_unfiled(out);
        self.record_receipt(m, true);
    }

    /// Remember the answer, here and, where the server keeps keywords, on
    /// the message itself as `$MDNSent`, which other mail apps read too.
    /// Microsoft 365 has no keywords: its categories are the user's own.
    fn record_receipt(&mut self, m: &Message, sent: bool) {
        self.receipts_answered
            .retain(|a| !a.key.eq_ignore_ascii_case(&m.message_id));
        self.receipts_answered.push(config::ReceiptAnswer {
            key: m.message_id.clone(),
            sent,
            at: crate::datefmt::now(),
        });
        config::save_receipts(&self.receipts_answered);
        // JMAP writes keywords in lower case (RFC 8621).
        match self.account_config(m.account_id).map(|c| c.protocol) {
            Some(config::Protocol::Graph) => {}
            Some(config::Protocol::Jmap) => self.set_tag(m, "$mdnsent", true),
            _ => self.set_tag(m, "$MDNSent", true),
        }
        self.refresh_receipt_banners();
    }

    /// Every check on screen again, the open message's header included,
    /// once the account's provider is known (see `Vouchers::note`).
    pub(super) fn refresh_shown_checks(&mut self) {
        let current = self
            .current
            .as_ref()
            .and_then(|m| Some((m.account_id, self.sender_cache.get(&body_key(m)).cloned()?)));
        if let Some((account_id, mut check)) = current {
            self.vouchers.gate(Some(account_id), &mut check);
            self.message_view
                .emit(MessageViewInput::SetSenderCheck(check));
        }
        self.refresh_receipt_banners();
    }

    /// Hand the readers their cards' checks again, so a banner comes and
    /// goes with the answer or the setting. Nothing is sent from here.
    pub(super) fn refresh_receipt_banners(&mut self) {
        let mut shown: Vec<Message> = self.current_thread.clone();
        if shown.is_empty() {
            shown.extend(self.current.clone());
        }
        for m in shown {
            let Some(check) = self.sender_cache.get(&body_key(&m)).cloned() else {
                continue;
            };
            let check = self.reader_check(&m, &check, false);
            self.message_view.emit(MessageViewInput::SenderCheckFor {
                account_id: m.account_id,
                id: m.id,
                check,
            });
        }
        let popouts: Vec<BodyKey> = self.popouts.values().map(|p| p.message).collect();
        for key in popouts {
            let (Some(m), Some(check)) =
                (self.held_message(key), self.sender_cache.get(&key).cloned())
            else {
                continue;
            };
            let check = self.reader_check(&m, &check, false);
            for p in self.popouts.values().filter(|p| p.message == key) {
                p.controller
                    .emit(MessageWindowInput::SetSenderCheck(check.clone()));
            }
        }
    }
}
