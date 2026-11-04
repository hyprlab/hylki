//! The app's side of the connection for AI assistants (`crate::mcp`): the
//! Settings page that starts and stops it and says what is allowed, the record
//! of what assistants did, and what the server asks of the main loop: raw
//! messages from the account's worker, and messages prepared in composers of
//! the app's own, the way Reply and Forward prepare them, each in a window of
//! its own that never takes over what the user is doing. Changing mail is in
//! `assistant_organize.rs`.

use std::collections::{HashMap, VecDeque};

use adw::prelude::*;
use relm4::prelude::*;
use tokio::sync::oneshot;

use crate::config::AssistantPolicy;
use crate::i18n::{i18n, i18n_f};
use crate::mcp::{Activity, AppCall, Draft, DraftKind, Original, Server, SharedPolicy};
use crate::models::Message;
use crate::ui::compose::{ComposeInput, ComposePrefill};
use crate::ui::preferences::{PrefInput, PrefOutput};
use crate::worker::MailRequest;

use super::compose_host::ComposeHost;
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::{forward_prefill, reply_all_prefill, reply_prefill, stage_attachments};

/// The most composers with an assistant's message open at once: more than
/// anyone checks with care, and an assistant gone wrong must not bury the
/// screen in windows.
const MAX_OPEN: usize = 5;

/// How many tool calls the record keeps.
const ACTIVITY_KEPT: usize = 200;

/// How often a message waiting for the user to be done looks again.
const WAITING_CHECK_SECS: u32 = 5;

/// A raw message the server waits on.
type WaitingRaw = oneshot::Sender<Result<Vec<u8>, String>>;

/// The connection while Settings has it on, and what outlasts it.
#[derive(Debug, Default)]
pub(super) struct Assistant {
    server: Option<Server>,
    /// Why it could not start, while it could not.
    error: Option<String>,
    /// What is allowed, as the server reads it.
    policy: SharedPolicy,
    /// Raw messages the server waits on, by `ExportRaw` token.
    raws: HashMap<u64, WaitingRaw>,
    /// What assistants did, newest last. In memory only.
    activity: VecDeque<Activity>,
}

impl AppModel {
    /// Listen for assistants, if Settings says to (at start-up).
    pub(super) fn start_assistant_if_on(&mut self, sender: &ComponentSender<Self>) {
        self.share_policy();
        if self.assistant_policy.enabled {
            self.start_assistant(sender);
        }
    }

    /// Hand the server what Settings says now.
    fn share_policy(&self) {
        *self
            .assistant
            .policy
            .write()
            .unwrap_or_else(|p| p.into_inner()) = self.assistant_policy.clone();
    }

    fn start_assistant(&mut self, sender: &ComponentSender<Self>) {
        if self.assistant.server.is_some() {
            return;
        }
        match Server::start(sender.input_sender().clone(), self.assistant.policy.clone()) {
            Ok(server) => {
                self.assistant.server = Some(server);
                self.assistant.error = None;
            }
            Err(e) => {
                tracing::warn!("mcp: could not listen: {e}");
                self.assistant.error = Some(e);
            }
        }
        self.push_assistant_activity();
    }

    fn stop_assistant(&mut self) {
        // Dropping it closes every connection and takes the socket away;
        // whatever was waiting is answered by its closing.
        self.assistant.server = None;
        self.assistant.error = None;
        self.assistant.raws.clear();
        self.push_assistant_activity();
    }

    /// What Settings says about the connection.
    pub(super) fn assistant_status_text(&self) -> String {
        let a = &self.assistant;
        if let Some(e) = &a.error {
            return i18n_f("Could not start: {error}", &[("error", e)]);
        }
        if a.server.is_none() {
            return i18n("Off");
        }
        match a.activity.back() {
            Some(last) => activity_line(last).0,
            None => i18n("No activity yet."),
        }
    }

    /// The record, newest first: each entry's title and subtitle.
    pub(super) fn assistant_activity_lines(&self) -> Vec<(String, String)> {
        self.assistant
            .activity
            .iter()
            .rev()
            .map(activity_line)
            .collect()
    }

    fn push_assistant_activity(&self) {
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::SetAssistantActivity {
                status: self.assistant_status_text(),
                lines: self.assistant_activity_lines(),
            });
        }
    }

    /// The Settings page changed something.
    pub(super) fn on_assistant_pref(&mut self, pref: PrefOutput, sender: ComponentSender<Self>) {
        let PrefOutput::SetAssistantPolicy(policy) = pref else {
            unreachable!("not routed to on_assistant_pref")
        };
        let policy: AssistantPolicy = *policy;
        let on = policy.enabled;
        if pref!(self.assistant_policy = policy) {
            self.share_policy();
            match (on, self.assistant.server.is_some()) {
                (true, false) => self.start_assistant(&sender),
                (false, true) => self.stop_assistant(),
                _ => {}
            }
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_assistant(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        let AppMsg::Assistant(call) = msg else {
            unreachable!("not routed to on_assistant")
        };
        match call {
            AppCall::FetchRaw {
                account_id,
                path,
                uid,
                done,
            } => self.fetch_raw_for_assistant(account_id, path, uid, done),
            AppCall::AskServer { account_id, ask } => {
                // With no worker the request goes nowhere, and the channel
                // closing tells the server so.
                self.send_to(account_id, MailRequest::AskServer(ask));
            }
            AppCall::Draft { draft, done } => self.open_assistant_draft(draft, done, &sender),
            AppCall::Organize {
                change,
                targets,
                done,
            } => {
                let organized = self.organize_for_assistant(change, targets);
                let _ = done.send(organized);
            }
            AppCall::Activity(activity) => {
                if self.assistant.activity.len() >= ACTIVITY_KEPT {
                    self.assistant.activity.pop_front();
                }
                self.assistant.activity.push_back(activity);
                self.push_assistant_activity();
            }
            AppCall::ShowWaiting => self.show_waiting_composers(),
        }
    }

    /// A message's raw bytes, asked of its account's worker as background
    /// work: what the user is waiting on goes first, and what it is doing is
    /// not interrupted. Nothing on this route can decrypt, and the bodies
    /// this app holds in memory, which may be what the user decrypted, are
    /// not looked at.
    fn fetch_raw_for_assistant(
        &mut self,
        account_id: u32,
        path: String,
        uid: u32,
        done: WaitingRaw,
    ) {
        if !self.workers.contains_key(&account_id) {
            let _ = done.send(Err("the account is off or offline".to_string()));
            return;
        }
        self.assistant.raws.retain(|_, d| !d.is_closed());
        let token = self.next_transfer;
        self.next_transfer += 1;
        self.assistant.raws.insert(token, done);
        self.send_to(
            account_id,
            MailRequest::ExportRaw {
                token,
                path,
                uid,
                for_reader: false,
            },
        );
    }

    /// A worker answered with a raw message: the server's if it asked for
    /// it, handed over (`None`); otherwise given back.
    pub(super) fn assistant_takes_raw(
        &mut self,
        token: u64,
        raw: Result<Vec<u8>, String>,
    ) -> Option<Result<Vec<u8>, String>> {
        match self.assistant.raws.remove(&token) {
            Some(done) => {
                let _ = done.send(raw);
                None
            }
            None => Some(raw),
        }
    }

    /// Composers with an assistant's message in them.
    fn assistant_composers(&self) -> usize {
        self.composers.iter().filter(|h| h.assistant).count()
    }

    /// Prepare the message an assistant asked for, as Hylki's own New,
    /// Reply, Reply All or Forward would, with its text where the user
    /// would have typed.
    fn open_assistant_draft(
        &mut self,
        draft: Draft,
        done: oneshot::Sender<Result<String, String>>,
        sender: &ComponentSender<Self>,
    ) {
        if self.assistant_composers() >= MAX_OPEN {
            let _ = done.send(Err(format!(
                "{MAX_OPEN} messages an assistant prepared are already open in Hylki. Ask the \
                 user to send or close some first."
            )));
            return;
        }
        match draft.kind {
            DraftKind::New {
                account_id,
                ref allowed,
            } => {
                let prefill = ComposePrefill {
                    to: draft.to.join(", "),
                    cc: draft.cc.join(", "),
                    bcc: draft.bcc.join(", "),
                    subject: draft.subject.clone(),
                    typed_html: crate::mcp::text_html(&draft.text),
                    assistant: true,
                    ..Default::default()
                };
                let (account, prefill) = match account_id {
                    Some(id) => (id, prefill),
                    None => self.new_message_within(allowed, prefill),
                };
                self.show_assistant_composer(account, prefill, done, sender);
            }
            DraftKind::Reply { ref original, all } => {
                let m = self.original_message(original);
                let prefill = if all {
                    reply_all_prefill(&m, &self.own_identities(m.account_id), &self.reply_header)
                } else {
                    reply_prefill(&m, &self.reply_header)
                };
                let mut prefill = self.reply_context(&m, prefill);
                prefill.to = with_more(&prefill.to, &draft.to);
                prefill.cc = with_more(&prefill.cc, &draft.cc);
                prefill.bcc = draft.bcc.join(", ");
                prefill.typed_html = crate::mcp::text_html(&draft.text);
                prefill.assistant = true;
                self.show_assistant_composer(m.account_id, prefill, done, sender);
            }
            DraftKind::Forward { ref original } => {
                let m = self.original_message(original);
                let mut prefill = self.quoting(&m, forward_prefill(&m));
                prefill.attachments = stage_attachments(
                    &format!("hylki-forward-{}-{}", m.account_id, m.id),
                    &original.attachments,
                );
                prefill.to = draft.to.join(", ");
                prefill.cc = draft.cc.join(", ");
                prefill.bcc = draft.bcc.join(", ");
                prefill.typed_html = crate::mcp::text_html(&draft.text);
                prefill.assistant = true;
                self.show_assistant_composer(m.account_id, prefill, done, sender);
            }
        }
    }

    /// Where a new message starts from: where the user's start from, as long
    /// as the assistant is allowed there, or else the first place it is.
    fn new_message_within(
        &self,
        allowed: &[u32],
        prefill: ComposePrefill,
    ) -> (u32, ComposePrefill) {
        let (account, mut prefill) = self.new_message_from(self.active_account(), prefill);
        if allowed.contains(&account) {
            return (account, prefill);
        }
        prefill.from_address = String::new();
        (allowed[0], prefill)
    }

    /// The original as the app holds messages: in its folder, by the id
    /// the app knows it by, with the body to quote.
    fn original_message(&self, original: &Original) -> Message {
        let mut m = original.message.clone();
        m.folder_id = self
            .folder_by_path(m.account_id, &original.path)
            .map_or(0, |f| f.id);
        m.body = original.body.clone();
        m
    }

    /// The composer, in a window of its own. With the user at work in
    /// Hylki it waits unshown, and a toast and a notification say it is
    /// ready, until they ask for it or go elsewhere; otherwise it opens
    /// without asking for the focus, and the notification follows if it did
    /// not get it.
    fn show_assistant_composer(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        done: oneshot::Sender<Result<String, String>>,
        sender: &ComponentSender<Self>,
    ) {
        let files = prefill.attachments.len();
        let (id, init) = self.build_compose_init(account_id, prefill, true, false);
        let controller = self.spawn_compose(init, sender);
        let window = self.compose_window(controller.widget(), id, sender);
        window.set_title(Some(&i18n("Message from an AI Assistant")));
        let busy_here = ours_in_front();
        if busy_here {
            self.show_toast(
                &adw::Toast::builder()
                    .title(i18n("An AI assistant prepared a message"))
                    .button_label(i18n("Show"))
                    .action_name(format!(
                        "app.{}",
                        crate::desktop::notify::PRESENT_COMPOSE_ACTION
                    ))
                    .timeout(10)
                    .build(),
            );
            crate::desktop::notify::assistant_ready();
            // The toast goes and a notification can be dismissed: the message
            // is not left where nothing leads to it. It shows as soon as the
            // user leaves, and they find it when they come back.
            let s = sender.input_sender().clone();
            gtk::glib::timeout_add_seconds_local(WAITING_CHECK_SECS, move || {
                if ours_in_front() {
                    return gtk::glib::ControlFlow::Continue;
                }
                let _ = s.send(AppMsg::Assistant(AppCall::ShowWaiting));
                gtk::glib::ControlFlow::Break
            });
        } else {
            window.set_visible(true);
            controller.emit(ComposeInput::FocusInitial);
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(900), || {
                if !ours_in_front() {
                    crate::desktop::notify::assistant_ready();
                }
            });
        }
        tracing::info!(
            "mcp: an assistant's message opened, with {files} file(s){}",
            if busy_here {
                ", waiting to be shown"
            } else {
                ""
            }
        );
        self.composers.push(ComposeHost {
            id,
            controller,
            window,
            assistant: true,
        });
        let note = if busy_here {
            "The message is ready in Hylki; the user was busy there, so it waits for them to open it. They check it and send it themselves."
        } else {
            "The message is open in Hylki in a window of its own. The user checks it and sends it themselves."
        };
        let mut note = note.to_string();
        if files > 0 {
            note.push_str(&format!(" The original's {files} attachment(s) go along."));
        }
        let _ = done.send(Ok(note));
    }

    /// Show the prepared messages still waiting.
    fn show_waiting_composers(&self) {
        for host in self
            .composers
            .iter()
            .filter(|h| h.assistant && !h.window.is_visible())
        {
            tracing::info!("mcp: showing a message that waited");
            host.window.set_visible(true);
            host.controller.emit(ComposeInput::FocusInitial);
        }
    }
}

/// An entry of the record, as (what it did, when and by which program, and
/// what at).
fn activity_line(a: &Activity) -> (String, String) {
    let done = match a.tool.as_str() {
        "list_accounts" => i18n("Listed accounts"),
        "list_folders" => i18n("Listed folders"),
        "list_messages" => i18n("Listed messages"),
        "search" => i18n("Searched mail"),
        "search_server" => i18n("Searched on the server"),
        "read_message" => i18n("Read a message"),
        "read_thread" => i18n("Read a conversation"),
        "list_attachments" => i18n("Listed attachments"),
        "read_attachment" => i18n("Read an attachment"),
        "find_contacts" => i18n("Looked up contacts"),
        "compose_message" => i18n("Prepared a message"),
        "reply_to_message" => i18n("Prepared a reply"),
        "forward_message" => i18n("Prepared a forward"),
        "set_read" => i18n("Changed read state"),
        "set_starred" => i18n("Changed stars"),
        "list_tags" => i18n("Listed tags"),
        "set_tag" => i18n("Changed tags"),
        "move_messages" => i18n("Moved messages"),
        "archive_messages" => i18n("Archived messages"),
        "trash_messages" => i18n("Moved messages to Trash"),
        other => other.to_string(),
    };
    let title = if a.ok {
        done
    } else {
        i18n_f("Refused or failed: {action}", &[("action", &done)])
    };
    let mut subtitle = crate::datefmt::time(a.at);
    if !a.client.is_empty() {
        subtitle.push_str(" · ");
        subtitle.push_str(&a.client);
    }
    if !a.detail.is_empty() {
        subtitle.push_str(" · ");
        subtitle.push_str(&a.detail);
    }
    (title, subtitle)
}

/// Whether any window of Hylki's has the focus.
fn ours_in_front() -> bool {
    let tops = gtk::Window::toplevels();
    (0..tops.n_items())
        .filter_map(|i| tops.item(i))
        .filter_map(|o| o.downcast::<gtk::Window>().ok())
        .any(|w| w.is_active())
}

/// A recipient list with `more` added, none twice.
fn with_more(list: &str, more: &[String]) -> String {
    let mut out: Vec<String> = crate::worker::parse_recipients(list)
        .into_iter()
        .map(|(name, addr)| crate::worker::format_recipient(&name, &addr))
        .collect();
    let mut have: Vec<String> = crate::worker::parse_recipients(list)
        .into_iter()
        .map(|(_, addr)| addr.to_lowercase())
        .collect();
    for r in more {
        for (name, addr) in crate::worker::parse_recipients(r) {
            if !have.contains(&addr.to_lowercase()) {
                have.push(addr.to_lowercase());
                out.push(crate::worker::format_recipient(&name, &addr));
            }
        }
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::with_more;

    #[test]
    fn recipients_added_to_a_reply_are_not_repeated() {
        assert_eq!(
            with_more(
                "Ada <ada@example.org>",
                &["ADA@example.org".into(), "Bo <bo@example.com>".into()]
            ),
            "Ada <ada@example.org>, Bo <bo@example.com>"
        );
        assert_eq!(with_more("", &["bo@example.com".into()]), "bo@example.com");
    }
}
