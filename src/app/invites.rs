//! Meeting invitations (#223): answering them and adding them to a calendar.

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f};
use crate::models::Message;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::first_recipient;
use super::send::machine_message;

impl AppModel {
    /// The addresses each account answers to: its own and every send-as
    /// alias, lowercased. What an invitation's attendee list is matched
    /// against, so the card can say where the reader's own answer stands
    /// (#223).
    pub(super) fn identities_map(&self) -> HashMap<u32, Vec<String>> {
        let mut out: HashMap<u32, Vec<String>> = HashMap::new();
        for (i, cfg) in self.effective_config().iter().enumerate() {
            let mut addresses = vec![cfg.email.trim().to_lowercase()];
            for alias in &cfg.aliases {
                if let Some((_, addr)) = first_recipient(&alias.identity) {
                    addresses.push(addr.to_lowercase());
                }
            }
            addresses.retain(|a| !a.is_empty());
            out.insert(i as u32 + 1, addresses);
        }
        out
    }

    /// The invitations answered so far, as the readers take them: event key
    /// → the PARTSTAT sent and when.
    pub(super) fn invite_answers_map(&self) -> HashMap<String, (String, i64)> {
        self.invite_answers
            .iter()
            .map(|a| (a.key.clone(), (a.status.clone(), a.at)))
            .collect()
    }

    /// One of a card's invitation buttons (#223).
    fn invite_action(
        &mut self,
        message: Message,
        invite: crate::models::Invite,
        action: crate::ui::message_view::InviteAction,
    ) {
        use crate::ui::message_view::InviteAction;
        match action {
            InviteAction::AddToCalendar => self.open_invite_in_calendar(&invite),
            InviteAction::Answer(rsvp) => self.answer_invite(&message, &invite, rsvp),
        }
    }

    /// "Add to Calendar": hand the event, as it arrived, to whatever
    /// application opens calendar files — the same road an attachment takes
    /// (`.ics` and all its portal handling), because that is exactly what
    /// this is.
    fn open_invite_in_calendar(&self, invite: &crate::models::Invite) {
        if invite.ics.is_empty() {
            self.notice_error(i18n("This message carries no calendar file to open."));
            return;
        }
        // Named for the meeting so the calendar's import dialog says what it
        // is about, rather than "invite.ics".
        let stem: String = invite
            .summary
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
            .collect();
        let name = match stem.trim() {
            "" => "invite.ics".to_string(),
            s => format!("{}.ics", &s[..s.len().min(60)]),
        };
        crate::ui::attachment_io::open_bytes(
            &name,
            invite.ics.as_bytes(),
            Some(self.window.upcast_ref::<gtk::Window>()),
        );
    }

    /// Accept, Maybe or Decline: the answer RFC 5546 asks for, mailed to the
    /// organizer from the address that was invited.
    ///
    /// No copy is filed in Sent — an RSVP is machinery, not correspondence,
    /// and the same is true of the unsubscribe requests the reader sends. A
    /// send that fails is reported and queued in the Outbox like any other.
    fn answer_invite(
        &mut self,
        message: &Message,
        invite: &crate::models::Invite,
        rsvp: crate::models::Rsvp,
    ) {
        let Some(organizer) = invite.organizer.as_ref().map(|o| o.email.clone()) else {
            self.notice_error(i18n("This invitation names no organiser to answer."));
            return;
        };
        let (from_alias, name, address) = self.invite_identity(message, invite);
        let ics = crate::mail::invite::reply_ics(invite, &name, &address, rsvp.partstat());
        let subject = match rsvp {
            crate::models::Rsvp::Accepted => {
                i18n_f("Accepted: {subject}", &[("subject", &invite.summary)])
            }
            crate::models::Rsvp::Tentative => {
                i18n_f("Tentative: {subject}", &[("subject", &invite.summary)])
            }
            crate::models::Rsvp::Declined => {
                i18n_f("Declined: {subject}", &[("subject", &invite.summary)])
            }
        };
        let who = if name.trim().is_empty() {
            address.clone()
        } else {
            name.clone()
        };
        let body = match rsvp {
            crate::models::Rsvp::Accepted => {
                i18n_f("{name} has accepted this invitation.", &[("name", &who)])
            }
            crate::models::Rsvp::Tentative => i18n_f(
                "{name} has tentatively accepted this invitation.",
                &[("name", &who)],
            ),
            crate::models::Rsvp::Declined => {
                i18n_f("{name} has declined this invitation.", &[("name", &who)])
            }
        };
        let mut out = machine_message(
            self.sending_account(message),
            from_alias,
            organizer,
            subject,
            body,
            // An answer belongs to the conversation the invitation started,
            // so the organizer's client files it with the request.
            message.message_id.clone(),
        );
        out.calendar = Some(crate::worker::CalendarPart {
            ics,
            method: "REPLY".to_string(),
        });
        self.send_unfiled(out);
        self.record_invite_answer(invite, rsvp);
        self.notice(match rsvp {
            crate::models::Rsvp::Accepted => i18n("Your answer has been sent: accepted."),
            crate::models::Rsvp::Tentative => i18n("Your answer has been sent: maybe."),
            crate::models::Rsvp::Declined => i18n("Your answer has been sent: declined."),
        });
    }

    /// The identity an answer leaves under: the address on the invitation's
    /// attendee list that belongs to this account (a calendar knows its
    /// guest by the address it invited), else the alias the message was
    /// addressed to, else the account itself. Returns the send-as alias for
    /// the wire (`None` for the account), the name to put on the attendee
    /// line, and the bare address.
    fn invite_identity(
        &self,
        message: &Message,
        invite: &crate::models::Invite,
    ) -> (Option<String>, String, String) {
        let Some(cfg) = self.account_config(self.sending_account(message)) else {
            return (None, String::new(), String::new());
        };
        let invited: Vec<String> = invite
            .attendees
            .iter()
            .map(|a| a.email.to_lowercase())
            .collect();
        for alias in &cfg.aliases {
            if let Some((name, addr)) = first_recipient(&alias.identity) {
                if invited.iter().any(|i| *i == addr.to_lowercase()) {
                    return (Some(alias.identity.clone()), name, addr);
                }
            }
        }
        if invited.iter().any(|i| *i == cfg.email.to_lowercase()) {
            return (None, cfg.name.clone(), cfg.email.clone());
        }
        // Not on the list under any address we know: answer as whoever the
        // message was addressed to, which is the unsubscribe rule and the
        // best guess there is.
        match self.unsubscribe_from(message) {
            (Some(alias), addr) => {
                let name = first_recipient(&alias).map(|(n, _)| n).unwrap_or_default();
                (Some(alias), name, addr)
            }
            (None, addr) => (None, cfg.name.clone(), addr),
        }
    }

    /// Remember an answer, and tell the readers so the banner says so.
    fn record_invite_answer(&mut self, invite: &crate::models::Invite, rsvp: crate::models::Rsvp) {
        let key = invite.key();
        if key.is_empty() {
            return;
        }
        self.invite_answers.retain(|a| a.key != key);
        self.invite_answers.push(config::InviteAnswer {
            key,
            status: rsvp.partstat().to_string(),
            at: crate::datefmt::now(),
            summary: invite.summary.clone(),
        });
        config::save_invite_answers(&self.invite_answers);
        let map = self.invite_answers_map();
        self.message_view
            .emit(MessageViewInput::SetInviteAnswers(map.clone()));
        self.popouts_emit(|| MessageWindowInput::SetInviteAnswers(map.clone()));
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_invites(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::InviteAction {
                message,
                invite,
                action,
            } => {
                self.invite_action(*message, *invite, action);
            }
            _ => unreachable!("not routed to on_invites"),
        }
    }
}
