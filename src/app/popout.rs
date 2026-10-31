//! Messages opened in their own window.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n;
use crate::models::{Attachment, FolderKind, Message};
use crate::ui::message_window::{
    MessageWindow, MessageWindowInit, MessageWindowInput, MessageWindowOutput,
};
use crate::worker::MailRequest;

use super::conversation::{body_key, BodyKey};
use super::hand_off::HandOffFiles;
use super::model::AppModel;
use super::msg::AppMsg;
use super::uri_queue::FILE_FOLDER;

/// A message displayed in its own top-level window (double-click to pop out).
pub(super) struct PopOut {
    pub(super) window: adw::Window,
    pub(super) controller: Controller<MessageWindow>,
    /// The popped-out message, where it lives (see [`body_key`]), for the
    /// events that arrive about it by folder and UID.
    pub(super) message: BodyKey,
    /// Every message the window shows, the same way, with its (account,
    /// id): the files of each are the window's to show.
    pub(super) members: Vec<(BodyKey, (u32, u32))>,
}

/// A pop-out's attachments: those in hand per message, and the messages
/// whose attachments are on their way.
type PopoutAttachments = (Vec<((u32, u32), Vec<Attachment>)>, Vec<(u32, u32)>);

/// What a pop-out window reports, as the message it means here. `key` is
/// the window's own (account, message).
fn popout_output_msg(key: (u32, u32), out: MessageWindowOutput) -> AppMsg {
    match out {
        MessageWindowOutput::Action { action, message } => {
            // The window's actions name the card they came from.
            AppMsg::RowAction {
                action,
                message,
                conversation: Vec::new(),
            }
        }
        MessageWindowOutput::AddToContacts { name, email } => {
            AppMsg::AddContactFrom { name, email }
        }
        MessageWindowOutput::LoadAttachments(message) => AppMsg::LoadAttachmentsFor(message),
        MessageWindowOutput::MarkSeen { account_id, id } => AppMsg::PopoutSeen { account_id, id },
        MessageWindowOutput::MoveTo { message, x, y } => {
            AppMsg::PopoutMoveTo { key, message, x, y }
        }
        MessageWindowOutput::Shortcut {
            action,
            message,
            thread,
        } => AppMsg::PopoutShortcut {
            action,
            message,
            thread,
        },
        MessageWindowOutput::SetTag {
            message,
            keyword,
            add,
        } => AppMsg::SetTag {
            message,
            keyword,
            add,
        },
        MessageWindowOutput::DeleteAttachment {
            message,
            attachment,
        } => AppMsg::DeletePopoutAttachment {
            key,
            message,
            attachment,
        },
        MessageWindowOutput::AllowSender(addr) => AppMsg::AllowSender(addr),
        MessageWindowOutput::ReloadBody(m) => AppMsg::ReloadBody(m),
        MessageWindowOutput::Notice(text) => AppMsg::Notice(text),
        MessageWindowOutput::Zoom(step) => AppMsg::ZoomMessage(step),
        MessageWindowOutput::Unsubscribe { message, info } => AppMsg::Unsubscribe { message, info },
        MessageWindowOutput::ReceiptAnswer { message, send } => {
            AppMsg::ReceiptAnswer { message, send }
        }
        MessageWindowOutput::InviteAction {
            message,
            invite,
            action,
        } => AppMsg::InviteAction {
            message,
            invite,
            action,
        },
        MessageWindowOutput::ComposeTo(addr) => AppMsg::ComposeTo(addr),
        MessageWindowOutput::CardMenu { message, x, y, hit } => AppMsg::CardMenu {
            message,
            x,
            y,
            hit,
            popout: Some(key),
        },
        MessageWindowOutput::Closed => AppMsg::PopoutClosed(key),
    }
}

impl AppModel {
    /// Sends every popped-out reader window the message `msg` builds, one
    /// built per window.
    pub(super) fn popouts_emit(&self, msg: impl Fn() -> MessageWindowInput) {
        for p in self.popouts.values() {
            p.controller.emit(msg());
        }
    }

    /// Present a read-only window showing raw message source (monospace).
    pub(super) fn show_source_window(&self, text: &str) {
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_text(text);
        let view = gtk::TextView::with_buffer(&buffer);
        view.set_editable(false);
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_top_margin(8);
        view.set_bottom_margin(8);

        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .child(&view)
            .build();

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&scroller));

        let window = adw::Window::builder()
            .transient_for(&self.window)
            .title(i18n("Message Source"))
            .default_width(720)
            .default_height(620)
            .content(&toolbar)
            .build();
        window.present();
    }

    /// Pop a message out into its own standalone window with a dedicated
    /// reader. `thread` is the whole conversation when `m` heads one (the
    /// window then shows every card); empty for a single message.
    pub(super) fn open_message_window(
        &mut self,
        m: Message,
        thread: Vec<Message>,
        sender: &ComponentSender<Self>,
    ) {
        let key = (m.account_id, m.id);
        // Already open? Just bring it forward.
        if let Some(p) = self.popouts.get(&key) {
            p.window.present();
            return;
        }
        let account_id = m.account_id;

        let cached_body = self.popout_body(&m);
        let needs_body = cached_body.is_none();

        let mut display = m.clone();
        display.unread = false;
        if let Some(body) = cached_body {
            display.body = body;
        }

        // Fetch the body unless the in-flight selection request already will
        // (single click precedes the double click), to avoid a duplicate fetch.
        // With the reading pane off a selection fetches nothing (#475).
        let already_loading = self.reader_shown()
            && self
                .current
                .as_ref()
                .is_some_and(|c| c.id == m.id && c.account_id == account_id);
        if needs_body && !already_loading {
            if let Some(path) = self.resolve_folder_path(&m) {
                self.send_to(
                    account_id,
                    MailRequest::LoadBody {
                        message_id: m.id,
                        path,
                        uid: m.uid,
                    },
                );
            }
        }

        let thread = self.popout_thread(&m, &display.body, thread);

        let shown: Vec<Message> = if thread.is_empty() {
            vec![display.clone()]
        } else {
            thread.clone()
        };
        let (atts, atts_loading) = self.popout_attachments(&shown);
        let members = shown
            .iter()
            .map(|m| (body_key(m), (m.account_id, m.id)))
            .collect();

        let init = self.popout_init(display, thread, needs_body, (atts, atts_loading));

        let controller = MessageWindow::builder()
            .launch(init)
            .forward(sender.input_sender(), move |out| {
                popout_output_msg(key, out)
            });

        let window = controller.widget().clone();
        window.set_transient_for(Some(&self.window));
        window.present();
        controller.emit(MessageWindowInput::SetUnsubscribed(self.unsubscribed_map()));
        controller.emit(MessageWindowInput::SetReceiptMarks(self.receipt_marks()));
        controller.emit(MessageWindowInput::SetIdentities(self.identities_map()));
        controller.emit(MessageWindowInput::SetInviteAnswers(
            self.invite_answers_map(),
        ));
        // A verdict already in hand reaches the window now, not only one that
        // arrives later: its seal, and a decrypted message's subject (#493).
        if let Some(check) = self.sender_cache.get(&body_key(&m)).cloned() {
            let check = self.reader_check(&m, &check, true);
            controller.emit(MessageWindowInput::SetSenderCheck(check));
        }

        self.popouts.insert(
            key,
            PopOut {
                window,
                controller,
                message: body_key(&m),
                members,
            },
        );
    }

    /// A body already fetched, so the window renders at once: on the
    /// message, on the reader's copy of it, or in the body cache.
    fn popout_body(&self, m: &Message) -> Option<String> {
        let account_id = m.account_id;
        if !m.body.is_empty() {
            Some(m.body.clone())
        } else if self
            .current
            .as_ref()
            .is_some_and(|c| c.id == m.id && c.account_id == account_id && !c.body.is_empty())
        {
            self.current.as_ref().map(|c| c.body.clone())
        } else {
            self.body_cache.get(&body_key(m)).cloned()
        }
    }

    /// The conversation for the window: the assembled cache when this
    /// thread has been opened before (cross-folder members and bodies
    /// included), else what the list handed over. Members still missing
    /// bodies get them fetched; the replies land via SetBody below.
    fn popout_thread(&self, m: &Message, body: &str, thread: Vec<Message>) -> Vec<Message> {
        let key = (m.account_id, m.id);
        let mut thread = if thread.len() > 1 {
            self.thread_cache
                .get(&body_key(m))
                .cloned()
                .unwrap_or(thread)
        } else {
            thread
        };
        for member in &mut thread {
            let mkey = (member.account_id, member.id);
            if mkey == key {
                member.body = body.to_string();
                member.unread = false;
                continue;
            }
            if member.body.is_empty() {
                if let Some(body) = self.body_cache.get(&body_key(member)) {
                    member.body = body.clone();
                } else if let Some(path) = self.resolve_folder_path(member) {
                    self.send_to(
                        member.account_id,
                        MailRequest::LoadBody {
                            message_id: member.id,
                            path,
                            uid: member.uid,
                        },
                    );
                }
            }
        }
        thread
    }

    /// Attachments, for every message the window shows: the in-memory
    /// cache where present; otherwise fetched (disk cache first, then the
    /// server) with the reply routed to this window, as the main reader
    /// does for a conversation.
    fn popout_attachments(&self, shown: &[Message]) -> PopoutAttachments {
        let mut atts: Vec<((u32, u32), Vec<Attachment>)> = Vec::new();
        let mut atts_loading: Vec<(u32, u32)> = Vec::new();
        for member in shown.iter().filter(|m| m.has_attachment) {
            let mkey = (member.account_id, member.id);
            if let Some(cached) = self.attachment_cache.get(&body_key(member)).cloned() {
                atts.push((mkey, cached));
            } else if let Some(path) = self.resolve_folder_path(member) {
                atts_loading.push(mkey);
                self.send_to(
                    member.account_id,
                    MailRequest::LoadAttachments {
                        message_id: member.id,
                        path,
                        uid: member.uid,
                        download: true,
                    },
                );
            }
        }
        (atts, atts_loading)
    }

    /// The pop-out's first state: the message, its conversation and
    /// attachments, and the reader settings as they stand.
    fn popout_init(
        &self,
        display: Message,
        thread: Vec<Message>,
        needs_body: bool,
        (atts, atts_loading): PopoutAttachments,
    ) -> MessageWindowInit {
        let account_id = display.account_id;
        let allow_remote = self.remote_allowed(&display);
        let folder_kind = self.folder_kind(display.account_id, display.folder_id);
        let init_from_file = display.folder_id == FILE_FOLDER;
        MessageWindowInit {
            message: display,
            thread,
            account_name: Some(self.account_name(account_id)),
            account_color: Some(self.account_color(account_id)),
            allow_remote,
            loading: needs_body,
            attachments: atts,
            attachments_loading: atts_loading,
            drawer_enabled: self.drawer_enabled,
            card_attachments: self.card_attachments,
            // A message from a file is in no folder: there is nothing to
            // star, file or delete on a server.
            toolbar: if init_from_file {
                config::ReaderToolbar {
                    left: vec![
                        config::ToolbarItem::Reply,
                        config::ToolbarItem::ReplyAll,
                        config::ToolbarItem::Forward,
                    ],
                    right: vec![config::ToolbarItem::Find, config::ToolbarItem::Print],
                }
            } else {
                self.reader_toolbar.clone()
            },
            toolbar_fold_right: self.toolbar_fold_right,
            in_junk: folder_kind == Some(FolderKind::Junk),
            restorable: matches!(folder_kind, Some(FolderKind::Trash | FolderKind::Junk)),
            snooze: if init_from_file {
                None
            } else {
                self.snooze_offer(account_id, folder_kind)
            },
            content_dark: self.message_theme.dark_override(),
            reader_style: self.reader_style(),
            reader_mode: self.effective_reader_mode(),
            zoom: self.zoom,
            zoom_default: self.zoom_default,
            reader_switch: self.reader_switch,
            theme_switch: self.theme_switch,
            account_chip: self.account_chip,
            reader_default: self.effective_reader_default(),
            tags: self.tags.clone(),
            pgp_labels: self.pgp_labels,
            find_bottom: self.reader_find_bottom,
            single_key: self.single_key.clone(),
            gmail_keys: self.gmail_keys.clone(),
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_popout(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::OpenMessageWindow { message: m, thread } => {
                // Drafts open in the editor rather than a read-only window,
                // and a template opens a new message made from it (#360).
                if self.is_drafts_folder(m.account_id, m.folder_id) {
                    self.open_draft(m, false, HandOffFiles::default(), &sender);
                } else if self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Templates)
                {
                    self.edit_as_new(m, &sender);
                } else {
                    self.mark_window_read(&m, thread.len() > 1);
                    // Popouts follow the reading pane's display order (#70).
                    let mut thread = thread;
                    if self.thread_newest_first {
                        thread.reverse();
                    }
                    self.open_message_window(m, thread, &sender);
                }
            }

            AppMsg::PopoutClosed(key) => {
                self.popouts.remove(&key);
            }

            AppMsg::AddContactFrom { name, email } => {
                self.show_add_contact_dialog(&name, &email, &sender);
            }

            AppMsg::PopoutMoveTo { key, message, x, y } => {
                let Some(window) = self.popouts.get(&key).map(|p| p.window.clone()) else {
                    return;
                };
                let accounts =
                    self.picker_accounts(message.account_id, self.resolve_folder_path(&message));
                if accounts.is_empty() {
                    return;
                }
                let s = sender.input_sender().clone();
                let picked = (*message).clone();
                let closing = window.clone();
                crate::ui::folder_picker::show_folder_picker(
                    &window,
                    x,
                    y,
                    accounts,
                    None,
                    move |account_id, dest, _| {
                        let _ = s.send(AppMsg::MoveMessagesTo {
                            account_id,
                            dest,
                            messages: vec![picked.clone()],
                        });
                        // As Archive and Delete do: the message has left.
                        closing.close();
                    },
                );
            }

            AppMsg::PopoutShortcut {
                action,
                message,
                thread,
            } => {
                self.run_popout_shortcut(action, *message, thread, &sender);
            }

            AppMsg::ViewSource => {
                if let Some(m) = self.current.clone() {
                    if let Some(path) = self.resolve_folder_path(&m) {
                        self.send_to(
                            m.account_id,
                            MailRequest::LoadSource {
                                message_id: m.id,
                                path,
                                uid: m.uid,
                            },
                        );
                    }
                }
            }
            _ => unreachable!("not routed to on_popout"),
        }
    }
}
