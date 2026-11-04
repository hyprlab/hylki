//! The app's side of the connection for AI assistants (`crate::mcp`): the
//! Settings switch that starts and stops it, and what the server asks of
//! the main loop: bodies downloaded as the reader downloads them, messages
//! prepared in composers of the app's own, the way Reply and Forward
//! prepare them, each in a window of its own that never takes over what
//! the user is doing, and the open composers read, or handed an edit for
//! the user to apply.

use adw::prelude::*;
use relm4::prelude::*;
use tokio::sync::oneshot;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::mcp::{AppCall, Draft, DraftEdit, DraftKind, OpenDraft, Original, Server};
use crate::models::Message;
use crate::ui::compose::{ComposeInput, ComposePrefill};
use crate::ui::preferences::{PrefInput, PrefOutput};
use crate::worker::MailRequest;

use super::compose_host::ComposeHost;
use super::conversation::{body_key, BodyKey};
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::{forward_prefill, reply_all_prefill, reply_prefill, stage_attachments};

/// The most composers with an assistant's message open at once: more than
/// anyone checks with care, and an assistant gone wrong must not bury the
/// screen in windows.
const MAX_OPEN: usize = 5;

/// A body the server waits on: where it is, (account, folder path, UID),
/// and where it goes.
type WaitingBody = ((u32, String, u32), oneshot::Sender<Option<String>>);
/// Where the server waits to hear which composer a draft opened in (its
/// id) and how, or why it did not open.
type Opened = oneshot::Sender<Result<(u32, String), String>>;

/// The connection while Settings has it on.
#[derive(Debug, Default)]
pub(super) struct Assistant {
    server: Option<Server>,
    /// Why it could not start, while it could not.
    error: Option<String>,
    /// Assistants connected now.
    sessions: u32,
    /// Bodies the server waits on.
    bodies: Vec<WaitingBody>,
    /// Forwards waiting on their original's attachments.
    forwards: Vec<WaitingForward>,
}

/// A forward held until the files it carries are downloaded.
#[derive(Debug)]
struct WaitingForward {
    key: BodyKey,
    message: Message,
    draft: Draft,
    done: Opened,
}

impl AppModel {
    /// Listen for assistants, if Settings says to (at start-up).
    pub(super) fn start_assistant_if_on(&mut self, sender: &ComponentSender<Self>) {
        if self.assistant_access {
            self.start_assistant(sender);
        }
    }

    fn start_assistant(&mut self, sender: &ComponentSender<Self>) {
        if self.assistant.server.is_some() {
            return;
        }
        match Server::start(sender.input_sender().clone()) {
            Ok(server) => {
                self.assistant.server = Some(server);
                self.assistant.error = None;
            }
            Err(e) => {
                tracing::warn!("mcp: could not listen: {e}");
                self.assistant.error = Some(e);
            }
        }
        self.assistant.sessions = 0;
        self.push_assistant_status();
    }

    fn stop_assistant(&mut self) {
        // Dropping it closes every connection and takes the socket away;
        // whatever was waiting is answered by its closing.
        self.assistant = Assistant::default();
        self.push_assistant_status();
    }

    /// What Settings says about the connection.
    pub(super) fn assistant_status_text(&self) -> String {
        let a = &self.assistant;
        match (&a.server, &a.error) {
            (_, Some(e)) => i18n_f("Could not start: {error}", &[("error", e)]),
            (None, None) => i18n("Off"),
            (Some(_), None) if a.sessions == 0 => i18n("On. No assistant is connected."),
            (Some(_), None) => ni18n_f(
                "On. {n} assistant is connected.",
                "On. {n} assistants are connected.",
                a.sessions,
                &[("n", &a.sessions.to_string())],
            ),
        }
    }

    fn push_assistant_status(&self) {
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::SetAssistantStatus(self.assistant_status_text()));
        }
    }

    /// The Settings switch.
    pub(super) fn on_assistant_pref(&mut self, pref: PrefOutput, sender: ComponentSender<Self>) {
        match pref {
            PrefOutput::SetAssistantAccess(on) => {
                if pref!(self.assistant_access = on) {
                    if on {
                        self.start_assistant(&sender);
                    } else {
                        self.stop_assistant();
                    }
                }
            }
            _ => unreachable!("not routed to on_assistant_pref"),
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_assistant(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        let AppMsg::Assistant(call) = msg else {
            unreachable!("not routed to on_assistant")
        };
        match call {
            AppCall::Connected(true) => {
                self.assistant.sessions += 1;
                self.push_assistant_status();
            }
            AppCall::Connected(false) => {
                self.assistant.sessions = self.assistant.sessions.saturating_sub(1);
                self.push_assistant_status();
            }
            AppCall::FetchBody {
                account_id,
                path,
                uid,
                done,
            } => self.fetch_body_for_assistant(account_id, path, uid, done),
            AppCall::Draft { draft, done } => self.open_assistant_draft(draft, done, &sender),
            AppCall::ReadDrafts { id, text, done } => {
                let _ = done.send(self.read_composers(id, text));
            }
            AppCall::EditDraft { id, edit, done } => {
                let _ = done.send(self.edit_composer(id, edit));
            }
        }
    }

    /// A body the server wants: the one in memory, or the worker's, which
    /// arrives as any other (see [`Self::hand_body_to_assistant`]).
    fn fetch_body_for_assistant(
        &mut self,
        account_id: u32,
        path: String,
        uid: u32,
        done: oneshot::Sender<Option<String>>,
    ) {
        let key = self
            .folder_by_path(account_id, &path)
            .map(|f| (account_id, f.id, uid));
        if let Some(body) = key.and_then(|k| self.body_cache.get(&k)) {
            let _ = done.send(Some(body.clone()));
            return;
        }
        if !self.workers.contains_key(&account_id) {
            // Disabled, or not connected: nothing would answer.
            let _ = done.send(None);
            return;
        }
        self.assistant.bodies.retain(|(_, d)| !d.is_closed());
        self.assistant
            .bodies
            .push(((account_id, path.clone(), uid), done));
        self.send_to(
            account_id,
            MailRequest::LoadBody {
                message_id: uid,
                path,
                uid,
            },
        );
    }

    /// A body arrived: hand it to the server where it waits for it.
    pub(super) fn hand_body_to_assistant(
        &mut self,
        account_id: u32,
        path: &str,
        message_id: u32,
        body: &str,
    ) {
        if self.assistant.bodies.is_empty() {
            return;
        }
        let (waiting, rest) = std::mem::take(&mut self.assistant.bodies)
            .into_iter()
            .partition(|((a, p, uid), _)| *a == account_id && p == path && *uid == message_id);
        self.assistant.bodies = rest;
        for (_, done) in waiting {
            let _ = done.send(Some(body.to_string()));
        }
    }

    /// Ask each open composer (or the one `id` names) what it holds; each
    /// answers on its own line, once its editor has handed its text over.
    fn read_composers(&self, id: Option<u32>, text: bool) -> Vec<oneshot::Receiver<OpenDraft>> {
        self.open_composers()
            .filter(|(cid, _)| id.is_none_or(|id| id == *cid))
            .map(|(_, controller)| {
                let (done, answer) = oneshot::channel();
                controller.emit(ComposeInput::AssistantRead { text, done });
                answer
            })
            .collect()
    }

    /// Put an assistant's edit before the user in composer `id`, to apply
    /// or reject there; `false` when no composer has that id.
    fn edit_composer(&self, id: u32, edit: DraftEdit) -> bool {
        let Some(controller) = self.compose_controller(id) else {
            return false;
        };
        tracing::info!("mcp: an assistant asked to edit composer {id}");
        controller.emit(ComposeInput::AssistantEdit(edit));
        true
    }

    /// Composers with an assistant's message in them.
    fn assistant_composers(&self) -> usize {
        self.composers.iter().filter(|h| h.assistant).count() + self.assistant.forwards.len()
    }

    /// Prepare the message an assistant asked for, as Hylki's own New,
    /// Reply, Reply All or Forward would, with its text where the user
    /// would have typed.
    fn open_assistant_draft(&mut self, draft: Draft, done: Opened, sender: &ComponentSender<Self>) {
        if self.assistant_composers() >= MAX_OPEN {
            let _ = done.send(Err(format!(
                "{MAX_OPEN} messages an assistant prepared are already open in Hylki. Ask the \
                 user to send or close some first."
            )));
            return;
        }
        match draft.kind {
            DraftKind::New { account_id } => {
                let prefill = ComposePrefill {
                    to: draft.to.join(", "),
                    cc: draft.cc.join(", "),
                    bcc: draft.bcc.join(", "),
                    subject: draft.subject,
                    typed_html: crate::mcp::text_html(&draft.text),
                    assistant: true,
                    ..Default::default()
                };
                let (account, prefill) = match account_id {
                    Some(id) => (id, prefill),
                    None => self.new_message_from(self.active_account(), prefill),
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
                // The files go along, as with Forward (#240): fetched
                // first when they are not in hand.
                let path = self.resolve_folder_path(&m);
                let key = body_key(&m);
                if let (true, false, Some(path)) = (
                    m.has_attachment,
                    self.attachment_cache.contains_key(&key),
                    path,
                ) {
                    self.send_to(
                        m.account_id,
                        MailRequest::LoadAttachments {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                            download: true,
                        },
                    );
                    self.assistant.forwards.push(WaitingForward {
                        key,
                        message: m,
                        draft,
                        done,
                    });
                    return;
                }
                self.assistant_forward(m, draft, done, sender);
            }
        }
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

    /// The files a waiting forward carries are in (or there are none):
    /// open it.
    pub(super) fn resume_assistant_forwards(
        &mut self,
        key: Option<BodyKey>,
        sender: &ComponentSender<Self>,
    ) {
        let Some(key) = key else { return };
        if self.assistant.forwards.is_empty() {
            return;
        }
        let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.assistant.forwards)
            .into_iter()
            .partition(|w| w.key == key);
        self.assistant.forwards = rest;
        for w in ready {
            if !w.done.is_closed() {
                self.assistant_forward(w.message, w.draft, w.done, sender);
            }
        }
    }

    fn assistant_forward(
        &mut self,
        mut m: Message,
        draft: Draft,
        done: Opened,
        sender: &ComponentSender<Self>,
    ) {
        if let Some(subject) = self.decrypted_subject(&m) {
            m.subject = subject;
        }
        let attachments = self
            .attachment_cache
            .get(&body_key(&m))
            .map(|items| {
                stage_attachments(&format!("hylki-forward-{}-{}", m.account_id, m.id), items)
            })
            .unwrap_or_default();
        let mut prefill = self.quoting(&m, forward_prefill(&m));
        prefill.attachments = attachments;
        prefill.to = draft.to.join(", ");
        prefill.cc = draft.cc.join(", ");
        prefill.bcc = draft.bcc.join(", ");
        prefill.typed_html = crate::mcp::text_html(&draft.text);
        prefill.assistant = true;
        self.show_assistant_composer(m.account_id, prefill, done, sender);
    }

    /// The composer, in a window of its own. With the user at work in
    /// Hylki it waits unshown, and a toast and a notification say it is
    /// ready; otherwise it opens without asking for the focus, and the
    /// notification follows if it did not get it.
    fn show_assistant_composer(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        done: Opened,
        sender: &ComponentSender<Self>,
    ) {
        self.leave_gallery();
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
        let _ = done.send(Ok((id, note)));
    }
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
