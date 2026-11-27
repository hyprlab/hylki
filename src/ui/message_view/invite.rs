//! A card's invitation banner (#223): the meeting, the answer given, and
//! the buttons to give one.

use super::html::attr_escape;
use super::snippets::inline_icon_svg;
use super::MessageView;
use crate::encoding::escape_html;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::Message;

/// A card's invitation banner (#223): the container, always emitted so a
/// meeting read after the paint can be patched into it (empty, it is hidden
/// by the stylesheet). `inner` is what [`MessageView::invite_inner_html`]
/// rendered.
pub(super) fn invite_row_html(key: (u32, u32), inner: &str) -> String {
    format!(
        "<div class=\"hylki-inv\" data-key=\"{}:{}\">{inner}</div>",
        key.0, key.1
    )
}

/// "Repeats weekly", from the event's `RRULE`. Only the frequency and the
/// interval are said: a rule can describe "the last Thursday of every other
/// month until June", and a banner that tried to spell that out would be a
/// paragraph. Anyone who needs the detail has the event in their calendar.
pub(super) fn repeat_text(rrule: &str) -> Option<String> {
    if rrule.trim().is_empty() {
        return None;
    }
    let mut freq = "";
    let mut interval: u32 = 1;
    for part in rrule.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = v.trim(),
            "INTERVAL" => interval = v.trim().parse().unwrap_or(1),
            _ => {}
        }
    }
    let n = &interval.to_string();
    Some(match (freq.to_ascii_uppercase().as_str(), interval) {
        ("DAILY", 1) => i18n("Repeats daily"),
        ("DAILY", _) => ni18n_f(
            "Repeats every {n} day",
            "Repeats every {n} days",
            interval,
            &[("n", n)],
        ),
        ("WEEKLY", 1) => i18n("Repeats weekly"),
        ("WEEKLY", _) => ni18n_f(
            "Repeats every {n} week",
            "Repeats every {n} weeks",
            interval,
            &[("n", n)],
        ),
        ("MONTHLY", 1) => i18n("Repeats monthly"),
        ("MONTHLY", _) => ni18n_f(
            "Repeats every {n} month",
            "Repeats every {n} months",
            interval,
            &[("n", n)],
        ),
        ("YEARLY", 1) => i18n("Repeats yearly"),
        ("YEARLY", _) => ni18n_f(
            "Repeats every {n} year",
            "Repeats every {n} years",
            interval,
            &[("n", n)],
        ),
        _ => i18n("Repeats"),
    })
}

/// What a `METHOD:REPLY` message says: one attendee answering, so the line
/// names them and what they said.
pub(super) fn reply_note(inv: &crate::models::Invite) -> String {
    use crate::models::Rsvp;
    let Some(who) = inv
        .attendees
        .iter()
        .find(|a| Rsvp::from_partstat(&a.status).is_some())
    else {
        return i18n("Somebody answered this invitation.");
    };
    let name = who.label();
    match Rsvp::from_partstat(&who.status) {
        Some(Rsvp::Accepted) => i18n_f("{name} accepted this invitation.", &[("name", name)]),
        Some(Rsvp::Tentative) => i18n_f(
            "{name} answered maybe to this invitation.",
            &[("name", name)],
        ),
        Some(Rsvp::Declined) => i18n_f("{name} declined this invitation.", &[("name", name)]),
        None => i18n("Somebody answered this invitation."),
    }
}

/// An answer the reader has already given: with the date when this app is
/// the one that sent it, without when the answer was only read off the
/// invitation (which records the standing, not the day).
pub(super) fn answer_note(plain: String, dated: String, at: i64) -> String {
    if at > 0 {
        dated
    } else {
        plain
    }
}

impl MessageView {
    /// What a card's invitation banner holds right now (#223): nothing for
    /// an ordinary message; otherwise the meeting — what, when, where and
    /// who — with the answer buttons when the sender is asking for one, and
    /// a line saying where the answer stands.
    pub(super) fn invite_inner_html(&self, m: &Message) -> String {
        let key = (m.account_id, m.id);
        let Some(inv) = self
            .member_checks
            .get(&key)
            .and_then(|c| c.invite.as_deref())
        else {
            return String::new();
        };
        let row = |text: &str| {
            if text.trim().is_empty() {
                String::new()
            } else {
                format!("<div class=\"hylki-inv-row\">{}</div>", escape_html(text))
            }
        };
        // The heading: the meeting's name, and what this message does to it.
        let title = if inv.summary.trim().is_empty() {
            i18n("Meeting")
        } else {
            inv.summary.clone()
        };
        let kind = invite_kind(inv);
        let when = invite_when(inv);
        let who = invite_who(inv);

        let answered = self.invite_answer_of(inv, m.account_id);
        let note = invite_note(inv, answered);

        let buttons = invite_buttons(key, inv, answered);

        format!(
            "<div class=\"hylki-inv-icon\">{icon}</div>\
             <div class=\"hylki-inv-body\">\
               <div class=\"hylki-inv-head\">\
                 <span class=\"hylki-inv-title{struck}\">{title}</span>{kind}\
               </div>{when}{where_}{who}{note}{acts}\
             </div>",
            icon = inline_icon_svg("x-office-calendar-symbolic"),
            struck = if inv.cancelled || inv.method == "CANCEL" {
                " off"
            } else {
                ""
            },
            title = escape_html(&title),
            kind = if kind.is_empty() {
                String::new()
            } else {
                format!(
                    "<span class=\"hylki-inv-kind\">{}</span>",
                    escape_html(&kind)
                )
            },
            when = row(&when),
            where_ = row(&inv.location),
            who = row(&who),
            note = if note.is_empty() {
                String::new()
            } else {
                format!("<div class=\"hylki-inv-note\">{}</div>", escape_html(&note))
            },
            acts = if buttons.is_empty() {
                String::new()
            } else {
                format!("<div class=\"hylki-inv-acts\">{buttons}</div>")
            },
        )
    }

    /// The answer already given to an invitation: the one this app sent and
    /// remembered, else whatever the organizer's own copy says about the
    /// reader's attendee line (a meeting answered in another client, or
    /// re-sent after the answer).
    fn invite_answer_of(
        &self,
        inv: &crate::models::Invite,
        account_id: u32,
    ) -> Option<(crate::models::Rsvp, i64)> {
        if let Some((status, at)) = self.invite_answers.get(&inv.key()) {
            if let Some(rsvp) = crate::models::Rsvp::from_partstat(status) {
                return Some((rsvp, *at));
            }
        }
        let mine = self.identities.get(&account_id)?;
        let me = inv.me(mine)?;
        // No date: the invitation says where the answer stands, not when it
        // was given.
        crate::models::Rsvp::from_partstat(&me.status).map(|r| (r, 0))
    }

    /// Redraw one card's invitation banner in the live document.
    pub(super) fn patch_invite(&self, account_id: u32, id: u32) {
        if !self.webview_ready || self.current.is_none() {
            return;
        }
        let Some(m) = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
        else {
            return;
        };
        let html = self.invite_inner_html(m);
        let js = format!(
            "(function(){{var d=document.querySelector('.hylki-inv[data-key=\"{account_id}:{id}\"]');\
             if(d)d.innerHTML={};}})()",
            serde_json::to_string(&html).unwrap_or_else(|_| "''".into()),
        );
        self.run_js(&js);
    }
}

/// What this message does to the meeting, for the banner's heading.
fn invite_kind(inv: &crate::models::Invite) -> String {
    if inv.cancelled || inv.method == "CANCEL" {
        i18n("Canceled")
    } else if inv.method == "REPLY" {
        i18n("Reply")
    } else if inv.sequence > 0 {
        i18n("Updated invitation")
    } else if inv.method == "REQUEST" || inv.method == "COUNTER" {
        i18n("Invitation")
    } else {
        String::new()
    }
}

/// When the meeting is, and how it repeats.
fn invite_when(inv: &crate::models::Invite) -> String {
    if inv.start == 0 {
        String::new()
    } else {
        let span = crate::datefmt::event_span(inv.start, inv.end, inv.all_day);
        match repeat_text(&inv.repeats) {
            Some(repeat) => format!("{span} · {repeat}"),
            None => span,
        }
    }
}

/// Who is behind the meeting, and how many are invited.
fn invite_who(inv: &crate::models::Invite) -> String {
    // Who is behind it, and how many are invited. The rooms on the
    // attendee list are not people and are not counted.
    let guests = inv.guests().count() as u32;
    match (&inv.organizer, guests) {
        (Some(o), 0) => i18n_f("Organized by {name}", &[("name", o.label())]),
        (Some(o), n) => format!(
            "{} · {}",
            i18n_f("Organized by {name}", &[("name", o.label())]),
            ni18n_f("{n} guest", "{n} guests", n, &[("n", &n.to_string())]),
        ),
        (None, 0) => String::new(),
        (None, n) => ni18n_f("{n} guest", "{n} guests", n, &[("n", &n.to_string())]),
    }
}

/// Where the answer stands: called off, someone else's reply, or the
/// reader's own answer.
fn invite_note(
    inv: &crate::models::Invite,
    answered: Option<(crate::models::Rsvp, i64)>,
) -> String {
    if inv.cancelled || inv.method == "CANCEL" {
        i18n("This meeting has been called off.")
    } else if inv.method == "REPLY" {
        // A reply from someone else: say who answered and how.
        reply_note(inv)
    } else {
        match answered {
            Some((crate::models::Rsvp::Accepted, at)) => answer_note(
                i18n("You accepted this invitation."),
                i18n_f(
                    "You accepted this invitation on {date}.",
                    &[("date", &crate::datefmt::day_month_year(at))],
                ),
                at,
            ),
            Some((crate::models::Rsvp::Tentative, at)) => answer_note(
                i18n("You answered maybe to this invitation."),
                i18n_f(
                    "You answered maybe to this invitation on {date}.",
                    &[("date", &crate::datefmt::day_month_year(at))],
                ),
                at,
            ),
            Some((crate::models::Rsvp::Declined, at)) => answer_note(
                i18n("You declined this invitation."),
                i18n_f(
                    "You declined this invitation on {date}.",
                    &[("date", &crate::datefmt::day_month_year(at))],
                ),
                at,
            ),
            None => String::new(),
        }
    }
}

/// The answer buttons, when the sender asks for one, and Add to
/// Calendar for anything that is still an event.
fn invite_buttons(
    key: (u32, u32),
    inv: &crate::models::Invite,
    answered: Option<(crate::models::Rsvp, i64)>,
) -> String {
    // The buttons. An invitation that has been called off, and an
    // answer from someone else, are not things to answer.
    let mut buttons = String::new();
    if inv.asks_for_an_answer() {
        use crate::models::Rsvp;
        let answer = |rsvp: Rsvp, verb: &str, label: String, title: String| {
            format!(
                "<button type=\"button\" class=\"hylki-inv-btn {verb}{on}\" \
                 data-key=\"{}:{}\" data-inv=\"{verb}\" title=\"{}\">{}</button>",
                key.0,
                key.1,
                attr_escape(&title),
                escape_html(&label),
                on = if answered.map(|(a, _)| a) == Some(rsvp) {
                    " on"
                } else {
                    ""
                },
            )
        };
        buttons.push_str(&answer(
            Rsvp::Accepted,
            "yes",
            i18n("Accept"),
            i18n("Tells the organizer you will be there"),
        ));
        buttons.push_str(&answer(
            Rsvp::Tentative,
            "maybe",
            i18n("Maybe"),
            i18n("Tells the organizer you might be there"),
        ));
        buttons.push_str(&answer(
            Rsvp::Declined,
            "no",
            i18n("Decline"),
            i18n("Tells the organizer you will not be there"),
        ));
    }
    // Handing the event to a calendar is worth offering for anything
    // that is still an event — including a published one nobody is
    // asking about.
    if !inv.cancelled && inv.method != "CANCEL" && inv.method != "REPLY" && !inv.ics.is_empty() {
        buttons.push_str(&format!(
            "<button type=\"button\" class=\"hylki-inv-btn cal\" data-key=\"{}:{}\" \
             data-inv=\"cal\" title=\"{}\">{}</button>",
            key.0,
            key.1,
            attr_escape(&i18n("Opens the meeting in your calendar application")),
            escape_html(&i18n("Add to Calendar")),
        ));
    }
    buttons
}
