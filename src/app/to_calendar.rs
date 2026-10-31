//! Create Event and Create Task (#564): a message made into an event or a
//! task, written to a calendar in Evolution Data Server, or handed to the
//! calendar application as an `.ics` file where EDS has none.

use adw::prelude::*;
use relm4::prelude::*;

use crate::calendar::{Item, ItemKind, Source};
use crate::i18n::i18n_f;
use crate::models::Message;
use crate::worker::MailRequest;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::sender_lines;

/// The steps of Create Event and Create Task, each back on the UI thread.
#[derive(Debug)]
pub(crate) enum ToCalendar {
    /// The calendars or task lists EDS offers, asked for off the UI thread;
    /// `None` with no EDS to ask.
    Sources {
        message: Box<Message>,
        kind: ItemKind,
        sources: Option<Vec<Source>>,
    },
    /// Writing the item to EDS finished.
    Saved(ItemKind, Result<(), String>),
}

impl AppModel {
    /// Start making `m` into an event or a task: its body first, if it is
    /// still on the server, then where the item can go.
    pub(super) fn make_calendar_item(
        &mut self,
        m: Message,
        kind: ItemKind,
        sender: &ComponentSender<Self>,
    ) {
        let mut m = self.with_cached_body(m);
        if let Some(subject) = self.decrypted_subject(&m) {
            m.subject = subject;
        }
        if m.body.is_empty() {
            if let Some(path) = self.resolve_folder_path(&m) {
                self.send_to(
                    m.account_id,
                    MailRequest::LoadBody {
                        message_id: m.id,
                        path,
                        uid: m.uid,
                    },
                );
                self.pending_to_calendar = Some((m, kind));
                return;
            }
        }
        let names = self.calendar_account_names();
        let input = sender.input_sender().clone();
        std::thread::spawn(move || {
            // The demo's mail is made up: nothing of it goes to a real
            // calendar.
            let sources = if demo_mode() {
                None
            } else {
                crate::calendar::sources(kind, names)
            };
            let _ = input.send(AppMsg::ToCalendar(ToCalendar::Sources {
                message: Box::new(m),
                kind,
                sources,
            }));
        });
    }

    /// The mail accounts' nicknames by address, for the calendars of the
    /// same accounts in EDS. An address two mail accounts share names
    /// neither.
    fn calendar_account_names(&self) -> crate::calendar::AccountNames {
        let mut names = crate::calendar::AccountNames::default();
        let mut shared = Vec::new();
        for cfg in self.effective_config() {
            let email = cfg.email.trim().to_lowercase();
            if names
                .nicknames
                .insert(email.clone(), cfg.display_label())
                .is_some()
            {
                shared.push(email);
            }
        }
        for email in shared {
            names.nicknames.remove(&email);
        }
        names
    }

    /// A body fetched for Create Event or Create Task has arrived.
    pub(super) fn resume_to_calendar(
        &mut self,
        account_id: u32,
        message_id: u32,
        body: &str,
        sender: &ComponentSender<Self>,
    ) {
        match self.pending_to_calendar.take() {
            Some((mut m, kind)) if m.account_id == account_id && m.id == message_id => {
                m.body = body.to_string();
                self.make_calendar_item(m, kind, sender);
            }
            other => self.pending_to_calendar = other,
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_to_calendar(&mut self, msg: ToCalendar, sender: ComponentSender<Self>) {
        match msg {
            ToCalendar::Sources {
                message,
                kind,
                sources,
            } => self.calendar_dialog(&message, kind, sources.unwrap_or_default(), &sender),
            ToCalendar::Saved(_, Ok(())) => {}
            ToCalendar::Saved(kind, Err(e)) => self.notice_error(match kind {
                ItemKind::Event => i18n_f("Could not add the event: {e}", &[("e", &e)]),
                ItemKind::Task => i18n_f("Could not add the task: {e}", &[("e", &e)]),
            }),
        }
    }

    fn calendar_dialog(
        &self,
        m: &Message,
        kind: ItemKind,
        sources: Vec<Source>,
        sender: &ComponentSender<Self>,
    ) {
        let body = crate::mail::quote::message_text(&m.body);
        let notes = format!("{}\n\n{}", sender_lines(m), body.trim());
        let start = crate::ui::calendar_item::Start {
            kind,
            summary: m.subject.clone(),
            notes,
            last_source: crate::config::load_calendar_choice(kind == ItemKind::Task),
        };
        // A pop-out's menu opens the dialog over the pop-out.
        let parent = relm4::main_application()
            .active_window()
            .unwrap_or_else(|| self.window.clone().upcast());
        let input = sender.input_sender().clone();
        let owner = parent.clone();
        crate::ui::calendar_item::show(&owner, start, sources, move |item, source| match source {
            Some(uid) => {
                crate::config::save_calendar_choice(kind == ItemKind::Task, &uid);
                let input = input.clone();
                std::thread::spawn(move || {
                    let result = crate::calendar::create(&uid, &item);
                    if let Err(e) = &result {
                        tracing::warn!("calendar: adding to {uid} failed: {e}");
                    }
                    let _ = input.send(AppMsg::ToCalendar(ToCalendar::Saved(kind, result)));
                });
            }
            None => open_in_calendar(&item, &parent),
        });
    }
}

/// No EDS: the item goes to whatever opens calendar files, as an
/// invitation's Add to Calendar does.
fn open_in_calendar(item: &Item, parent: &gtk::Window) {
    let ics =
        crate::calendar::calendar_file(item, &crate::calendar::new_uid(), crate::datefmt::now());
    let stem: String = item
        .summary
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .take(60)
        .collect();
    let name = match (stem.trim(), item.kind) {
        ("", ItemKind::Event) => "event.ics".to_string(),
        ("", ItemKind::Task) => "task.ics".to_string(),
        (s, _) => format!("{s}.ics"),
    };
    crate::ui::attachment_io::open_bytes(&name, ics.as_bytes(), Some(parent));
}
