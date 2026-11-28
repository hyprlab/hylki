//! The Calendar (#715) in the app: showing it in place of the mail panes,
//! and doing what it asks of the rest of the app, which is mail: an answer
//! to an invitation, an invitation to the people on a meeting made here, a
//! message to them.

use relm4::prelude::*;

use crate::calendar::AccountNames;
use crate::config::{self, AccountConfig};
use crate::i18n::{i18n, i18n_f};
use crate::models::InvitePerson;
use crate::ui::calendar_page::{CalendarInit, CalendarInput, CalendarOutput};
use crate::ui::compose::ComposePrefill;

use super::model::AppModel;
use super::msg::AppMsg;
use super::send::machine_message;

/// The addresses each account answers to, and who a meeting made here is
/// organized by: the first account that sends.
fn identities(configs: &[AccountConfig]) -> (Vec<String>, Option<InvitePerson>) {
    let mut me = Vec::new();
    for c in configs {
        me.push(c.email.trim().to_lowercase());
        for a in &c.aliases {
            let (_, addr) = config::split_identity(&a.identity);
            me.push(addr.trim().to_lowercase());
        }
    }
    me.retain(|a| !a.is_empty());
    me.dedup();
    let organizer = configs
        .iter()
        .find(|c| c.enabled && !c.is_local() && !c.email.trim().is_empty())
        .map(|c| InvitePerson {
            name: c.name.clone(),
            email: c.email.trim().to_string(),
            status: "ACCEPTED".into(),
            rsvp: false,
            resource: false,
        });
    (me, organizer)
}

/// What the calendar starts with.
pub(super) fn calendar_init(configs: &[AccountConfig]) -> CalendarInit {
    let (me, organizer) = identities(configs);
    CalendarInit {
        names: AccountNames::default(),
        me,
        organizer,
    }
}

impl AppModel {
    /// Show the calendar in place of the mail panes.
    pub(super) fn show_calendar_view(&mut self) {
        self.close_sidebar_peek();
        self.mirror_selection(crate::ui::sidebar::Sel::Calendar);
        self.showing_outbox = false;
        self.leave_gallery();
        self.showing_contacts = false;
        self.showing_calendar = true;
        let (me, organizer) = identities(self.effective_config());
        self.calendar_page.emit(CalendarInput::Identities {
            me,
            organizer,
            names: self.calendar_account_names(),
        });
        self.calendar_page.emit(CalendarInput::Shown);
    }

    /// The account that sends as `address`: the one with that address or
    /// alias, else the first that sends.
    fn account_for(&self, address: &str) -> (u32, Option<String>) {
        let address = address.trim().to_lowercase();
        let sends = |c: &&AccountConfig| c.enabled && !c.is_local();
        for (i, c) in self.config.iter().enumerate().filter(|(_, c)| sends(c)) {
            if c.email.trim().eq_ignore_ascii_case(&address) {
                return (i as u32 + 1, None);
            }
            for a in &c.aliases {
                let (_, addr) = config::split_identity(&a.identity);
                if addr.trim().eq_ignore_ascii_case(&address) {
                    return (i as u32 + 1, Some(a.identity.clone()));
                }
            }
        }
        let first = self
            .config
            .iter()
            .position(|c| c.enabled && !c.is_local())
            .map_or(1, |i| i as u32 + 1);
        (first, None)
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_calendar(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        let out = match msg {
            AppMsg::ShowCalendar => {
                self.show_calendar_view();
                return;
            }
            AppMsg::Calendar(out) => out,
            _ => unreachable!("not routed to on_calendar"),
        };
        match out {
            CalendarOutput::ToggleSidebar => sender.input(AppMsg::ToggleSidebar),
            CalendarOutput::Notice { text, error } => {
                if error {
                    self.notice_error(text);
                } else {
                    self.notice(text);
                }
            }
            CalendarOutput::Answer {
                ics,
                organizer,
                summary,
                status,
                from,
            } => {
                let (account, alias) = self.account_for(&from);
                let (subject, body) = match status.as_str() {
                    "ACCEPTED" => (
                        i18n_f("Accepted: {subject}", &[("subject", &summary)]),
                        i18n_f("{name} has accepted this invitation.", &[("name", &from)]),
                    ),
                    "TENTATIVE" => (
                        i18n_f("Tentative: {subject}", &[("subject", &summary)]),
                        i18n_f(
                            "{name} has tentatively accepted this invitation.",
                            &[("name", &from)],
                        ),
                    ),
                    _ => (
                        i18n_f("Declined: {subject}", &[("subject", &summary)]),
                        i18n_f("{name} has declined this invitation.", &[("name", &from)]),
                    ),
                };
                let mut m = machine_message(account, alias, organizer, subject, body, String::new());
                m.calendar = Some(crate::worker::CalendarPart {
                    ics,
                    method: "REPLY".to_string(),
                });
                self.send_unfiled(m);
                self.notice(match status.as_str() {
                    "ACCEPTED" => i18n("Your answer has been sent: accepted."),
                    "TENTATIVE" => i18n("Your answer has been sent: maybe."),
                    _ => i18n("Your answer has been sent: declined."),
                });
            }
            CalendarOutput::Invite {
                ics,
                to,
                summary,
                body,
                from,
                cancel,
            } => {
                let (account, alias) = self.account_for(&from);
                let subject = if cancel {
                    i18n_f("Cancelled: {subject}", &[("subject", &summary)])
                } else {
                    i18n_f("Invitation: {subject}", &[("subject", &summary)])
                };
                let mut m = machine_message(account, alias, to.join(", "), subject, body, String::new());
                m.calendar = Some(crate::worker::CalendarPart {
                    ics,
                    method: if cancel { "CANCEL" } else { "REQUEST" }.to_string(),
                });
                self.send_unfiled(m);
                let n = to.len() as u32;
                self.notice(if cancel {
                    crate::i18n::ni18n_f(
                        "The cancellation has been sent to {n} person.",
                        "The cancellation has been sent to {n} people.",
                        n,
                        &[("n", &n.to_string())],
                    )
                } else {
                    crate::i18n::ni18n_f(
                        "The invitation has been sent to {n} person.",
                        "The invitation has been sent to {n} people.",
                        n,
                        &[("n", &n.to_string())],
                    )
                });
            }
            CalendarOutput::Compose { to, subject } => {
                self.compose_addressed(
                    ComposePrefill {
                        to: to.join(", "),
                        subject,
                        ..Default::default()
                    },
                    &sender,
                );
            }
            CalendarOutput::AddAccount => {
                sender.input(AppMsg::OpenSettings);
                sender.input(AppMsg::ShowSettingsPage("contacts".into()));
            }
        }
    }
}

/// Put an invitation's event in the calendar where new events go, with the
/// user's answer on it when there is one (`status`, a PARTSTAT). Answers the
/// calendar's name, or why it could not.
fn add_invite(ics: &str, status: Option<&str>, me: &[String]) -> Result<String, String> {
    use crate::calendar::store;
    let backend = store::backend();
    let prefs = crate::calendar::prefs::CalendarPrefs::load();
    let cals: Vec<_> = store::calendars(backend, AccountNames::default())?
        .into_iter()
        .filter(|c| c.writable && !c.tasks)
        .collect();
    let cal = cals
        .iter()
        .find(|c| c.uid == prefs.default_calendar)
        .or(cals.first())
        .ok_or_else(|| i18n("No calendar here takes new events."))?;
    let text = match status {
        Some(s) => {
            let mut node = crate::calendar::tree::Node::parse(ics).ok_or_else(|| i18n("The file holds no events."))?;
            for child in node.children.iter_mut().filter(|c| c.name == "VEVENT") {
                child.lines = child
                    .lines
                    .iter()
                    .map(|l| {
                        let mine = crate::mail::invite::ics::split_line(l).is_some_and(|p| {
                            p.name.eq_ignore_ascii_case("ATTENDEE")
                                && me.iter().any(|m| {
                                    p.raw.to_lowercase().trim_start_matches("mailto:") == m.as_str()
                                })
                        });
                        if mine {
                            crate::calendar::edit::set_partstat(l, s)
                        } else {
                            l.clone()
                        }
                    })
                    .collect();
            }
            node.write()
        }
        None => ics.to_string(),
    };
    store::import(backend, cal, &text)?;
    Ok(cal.full_name())
}

impl AppModel {
    /// An invitation's event, put in the calendar (#715): from "Add to
    /// Calendar", and when the user accepts. `quiet` says nothing when it
    /// worked.
    pub(super) fn invite_to_calendar(&self, ics: String, status: Option<String>, quiet: bool) {
        let (me, _) = identities(self.effective_config());
        let notices = self.notifications.sender().clone();
        let page = self.calendar_page.sender().clone();
        gtk::glib::spawn_future_local(async move {
            let r = gtk::gio::spawn_blocking(move || add_invite(&ics, status.as_deref(), &me))
                .await
                .unwrap_or_else(|_| Err(String::new()));
            let (text, error) = match r {
                Ok(_) if quiet => (None, false),
                Ok(name) => (Some(i18n_f("Added to {calendar}", &[("calendar", &name)])), false),
                Err(e) => (Some(i18n_f("Could not add it to the calendar: {e}", &[("e", &e)])), true),
            };
            if let Some(text) = text {
                let _ = notices.send(crate::ui::notifications::NotifyInput::Push {
                    text,
                    error,
                    connectivity: false,
                });
            }
            let _ = page.send(CalendarInput::Reload);
        });
    }
}
