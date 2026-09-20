//! A message popped out into its own top-level window: a standalone reader that
//! carries the exact same toolbar actions as the main window's reader pane.
//!
//! The window owns its own [`MessageView`] and attachment state, but defers the
//! real work (reply, move, load attachments, …) back to the app via outputs so
//! behaviour stays identical to the main window.

use adw::prelude::*;
use relm4::prelude::*;

use crate::models::{Attachment, Message};
use crate::ui::message_list::RowAction;
use crate::ui::message_view::{MessageView, MessageViewInput, MessageViewOutput};
use crate::i18n::i18n;

/// Everything needed to open a popout reader for one message.
#[derive(Debug)]
pub struct MessageWindowInit {
    pub message: Message,
    /// The whole conversation (oldest first) when the popped-out message heads
    /// one; empty for a plain single message.
    pub thread: Vec<Message>,
    pub account_name: Option<String>,
    pub account_color: Option<String>,
    pub allow_remote: bool,
    /// The body is still being fetched — show a spinner.
    pub loading: bool,
    pub attachments: Vec<Attachment>,
    /// Attachments exist on the server but aren't downloaded yet.
    pub attachments_available: bool,
    pub attachments_loading: bool,
    /// Message-content theme override (`None` follows the system).
    pub content_dark: Option<bool>,
    /// The reader's own fonts and colours over the senders' (#56).
    pub reader_style: crate::config::ReaderStyle,
    /// Reader View (the main window's header toggle), followed here too.
    pub reader_mode: bool,
    /// Whether the Reader View switch is shown at all.
    pub reader_switch: bool,
    /// What Reader View does when a message is opened.
    pub reader_default: crate::config::ReaderDefault,
    /// The tags (#71), for the cards' chips.
    pub tags: Vec<crate::config::Tag>,
}

pub struct MessageWindow {
    msg: Message,
    /// What the reader renders: the whole conversation, or just `[msg]`.
    thread: Vec<Message>,
    view: Controller<MessageView>,
    account_name: Option<String>,
    account_color: Option<String>,
    allow_remote: bool,
    loading: bool,
    attachments: Vec<Attachment>,
    attachments_available: bool,
    attachments_loading: bool,
    attach_list: gtk::Box,
}

#[derive(Debug)]
pub enum MessageWindowInput {
    /// Print this message (Ctrl+P), the same as in the main window.
    Print,
    /// Preview it as a PDF (Ctrl+Shift+P).
    PrintPreview,
    /// No-op (unreachable output mapping).
    Ignore,
    /// A message body arrived from the server — the popped-out message itself
    /// or, in a conversation window, any member of the thread.
    SetBody { account_id: u32, id: u32, body: String },
    /// The sender-authentication verdict for this message.
    SetSenderCheck(Box<crate::models::SenderCheck>),
    /// Reflect a star toggle that happened elsewhere (or came back from the app).
    SetStarred(bool),
    /// The message's keywords changed (#71).
    SetKeywords(Vec<String>),
    /// The tag definitions changed.
    SetTags(Vec<crate::config::Tag>),
    /// Downloaded attachments are now available.
    SetAttachments(Vec<Attachment>),
    /// Attachments exist but need an explicit download.
    AttachmentsPending,
    /// Update the message-content theme (`None` follows the system).
    SetContentTheme(Option<bool>),
    /// The reader's own fonts and colours changed (#56).
    SetReaderStyle(crate::config::ReaderStyle),
    SetReaderMode(bool),
    SetReaderSwitchShown(bool),
    SetReaderDefault(crate::config::ReaderDefault),
    /// This window's own Reader View toggle was flipped: handed up to the
    /// app, which owns the preference and pushes it back to every reader.
    ReaderMode(bool),
    /// What one of the user's own mailboxes shows changed (#189).
    FacesChanged,
    // ---- toolbar actions ----
    Reply,
    ReplyAll,
    Forward,
    AddToContacts,
    ToggleStar,
    Delete,
    Archive,
    Spam,
    ViewSource,
    LoadAttachmentsNow,
    OpenAttachment(usize),
    SaveAllAttachments,
    // ---- from the embedded reader ----
    /// Fetch a message's body again (its OpenPGP verdict changed, #133).
    ReloadBody(Box<Message>),
    /// A toast for the main window.
    Notice(String),
    AllowSender(String),
    /// The message card's own Reply/Reply all/Forward button.
    /// An action chosen on one card — in a conversation window, possibly a
    /// member other than the popped-out message itself.
    CardAction { action: RowAction, message: Box<Message> },
    /// A card's "Add sender to Contacts" — for that card's message.
    ContactFor(Box<Message>),
    /// An email address in a card header was clicked — compose to it.
    ComposeTo(String),
    /// "Add to Contacts" from an address's right-click menu.
    AddContactAddr(String),
    /// A card's Unsubscribe button — handed to the app, which owns the
    /// request.
    Unsubscribe { message: Box<Message>, info: Box<crate::models::Unsubscribe> },
    /// The lists left so far, for the cards' banners.
    SetUnsubscribed(std::collections::HashMap<String, i64>),
    /// Where a card's unsubscribe request stands.
    UnsubscribeState {
        account_id: u32,
        id: u32,
        state: Option<crate::ui::message_view::UnsubState>,
    },
    /// A card's invitation button — handed to the app, which owns the
    /// answer (#223).
    InviteAction {
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: crate::ui::message_view::InviteAction,
    },
    /// The addresses each account answers to, for the invitation banners.
    SetIdentities(std::collections::HashMap<u32, Vec<String>>),
    /// The invitations answered so far, for those banners.
    SetInviteAnswers(std::collections::HashMap<String, (String, i64)>),
}

#[derive(Debug)]
pub enum MessageWindowOutput {
    /// A card's Unsubscribe button: leave the list, by these handles.
    Unsubscribe { message: Box<Message>, info: Box<crate::models::Unsubscribe> },
    /// A card's invitation button: answer the organiser, or hand the
    /// meeting to a calendar application (#223).
    InviteAction {
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: crate::ui::message_view::InviteAction,
    },
    /// Reader View flipped from this window's subject block.
    ReaderMode(bool),
    /// A per-message action handled exactly like a list/context-menu action.
    Action { action: RowAction, message: Box<Message> },
    /// Add this sender to Contacts.
    AddToContacts { name: String, email: String },
    /// Download this message's attachments from the server.
    LoadAttachments(Box<Message>),
    /// Open a single attachment.
    OpenAttachment(Box<Attachment>),
    /// Save every attachment.
    SaveAllAttachments(Vec<Attachment>),
    /// Persist a remote-content allowlist entry.
    /// Fetch a message's body again (its OpenPGP verdict changed, #133).
    ReloadBody(Box<Message>),
    /// A toast for the main window.
    Notice(String),
    AllowSender(String),
    /// An email address in a card header was clicked — compose to it.
    ComposeTo(String),
    /// The window was closed.
    Closed,
}

#[relm4::component(pub)]
impl Component for MessageWindow {
    type Init = MessageWindowInit;
    type Input = MessageWindowInput;
    type Output = MessageWindowOutput;
    type CommandOutput = ();

    view! {
        adw::Window {
            set_title: Some(&title_text(&model.msg)),
            set_default_width: 720,
            set_default_height: 820,

            connect_close_request[sender] => move |_| {
                let _ = sender.output(MessageWindowOutput::Closed);
                gtk::glib::Propagation::Proceed
            },

            #[wrap(Some)]
            set_content = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    add_css_class: "flat",
                    #[wrap(Some)]
                    set_title_widget = &gtk::Label {
                        #[watch]
                        set_label: &title_text(&model.msg),
                        add_css_class: "pane-title",
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-reply-sender-symbolic",
                        set_tooltip_text: Some(i18n("Reply").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::Reply,
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-reply-all-symbolic",
                        set_tooltip_text: Some(i18n("Reply All").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::ReplyAll,
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-forward-symbolic",
                        set_tooltip_text: Some(i18n("Forward").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::Forward,
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-contact-new-symbolic",
                        set_tooltip_text: Some(i18n("Add sender to Contacts").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::AddToContacts,
                    },
                    pack_start = &gtk::Button {
                        set_tooltip_text: Some(i18n("Flag").as_str()),
                        set_icon_name: "co.hyprlab.Hylki-non-starred-symbolic",
                        #[watch]
                        set_css_classes: if model.msg.starred {
                            &["flat", "star-active"]
                        } else {
                            &["flat"]
                        },
                        connect_clicked => MessageWindowInput::ToggleStar,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-user-trash-symbolic",
                        set_tooltip_text: Some(i18n("Delete").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::Delete,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-code-symbolic",
                        set_tooltip_text: Some(i18n("View Source").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::ViewSource,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-mark-junk-symbolic",
                        set_tooltip_text: Some(i18n("Mark as Spam").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::Spam,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-archive-symbolic",
                        set_tooltip_text: Some(i18n("Archive").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageWindowInput::Archive,
                    },
                    pack_end = &gtk::Spinner {
                        set_valign: gtk::Align::Center,
                        set_tooltip_text: Some(i18n("Downloading attachments…").as_str()),
                        #[watch]
                        set_spinning: model.attachments_loading,
                        #[watch]
                        set_visible: model.attachments_loading,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-folder-download-symbolic",
                        set_tooltip_text: Some(i18n("Load attachments from server").as_str()),
                        add_css_class: "flat",
                        add_css_class: "attach-present",
                        #[watch]
                        set_visible: model.attachments_available && !model.attachments_loading,
                        connect_clicked => MessageWindowInput::LoadAttachmentsNow,
                    },
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "co.hyprlab.Hylki-mail-attachment-symbolic",
                        set_tooltip_text: Some(i18n("Attachments").as_str()),
                        add_css_class: "flat",
                        add_css_class: "attach-present",
                        #[watch]
                        set_visible: !model.attachments.is_empty(),
                        #[wrap(Some)]
                        set_popover = &gtk::Popover {
                            #[local_ref]
                            attach_list -> gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 4,
                                set_width_request: 260,
                            },
                        },
                    },
                },
                #[wrap(Some)]
                set_content = model.view.widget(),
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let view = MessageView::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                MessageViewOutput::AllowSender(addr) => MessageWindowInput::AllowSender(addr),
                // A popout is already its own window; opening another from its
                // single card is a no-op that keeps the match total.
                MessageViewOutput::OpenWindow(_) => MessageWindowInput::Ignore,
                // Card actions carry their own message, so in a conversation
                // window each card acts on its member — the head card behaves
                // exactly like the toolbar's buttons.
                MessageViewOutput::CardAction { action, message } => {
                    MessageWindowInput::CardAction { action, message }
                }
                MessageViewOutput::ContactSender(m) => MessageWindowInput::ContactFor(m),
                MessageViewOutput::MarkSeen { .. } => MessageWindowInput::Ignore,
                // The standalone window has no list menu, nor a folder picker.
                MessageViewOutput::CardMenu { .. } | MessageViewOutput::CardMoveTo { .. } => {
                    MessageWindowInput::Ignore
                }
                MessageViewOutput::SelectCards(_) => MessageWindowInput::Ignore,
                // A window's cards are never given attachment rows (#213).
                MessageViewOutput::AttachmentAction { .. } => MessageWindowInput::Ignore,
                // The pop-out keeps its own view's state; the main window's
                // per-message record is not its to write.
                MessageViewOutput::SetRemote { .. } => MessageWindowInput::Ignore,
                MessageViewOutput::ComposeTo(addr) => MessageWindowInput::ComposeTo(addr),
                MessageViewOutput::ReloadBody(m) => MessageWindowInput::ReloadBody(m),
                MessageViewOutput::Notice(text) => MessageWindowInput::Notice(text),
                MessageViewOutput::ReaderMode(on) => MessageWindowInput::ReaderMode(on),
                MessageViewOutput::Unsubscribe { message, info } => {
                    MessageWindowInput::Unsubscribe { message, info }
                }
                MessageViewOutput::InviteAction { message, invite, action } => {
                    MessageWindowInput::InviteAction { message, invite, action }
                }
                MessageViewOutput::AddContactAddr(addr) => {
                    MessageWindowInput::AddContactAddr(addr)
                }
            });
        // Apply the message-content theme before the first render.
        view.emit(MessageViewInput::SetContentTheme(init.content_dark));
        view.emit(MessageViewInput::SetReaderStyle(init.reader_style.clone()));
        view.emit(MessageViewInput::SetReaderMode(init.reader_mode));
        view.emit(MessageViewInput::SetReaderSwitchShown(init.reader_switch));
        view.emit(MessageViewInput::SetReaderDefault(init.reader_default));
        view.emit(MessageViewInput::SetTags(init.tags.clone()));

        let thread = if init.thread.is_empty() {
            vec![init.message.clone()]
        } else {
            init.thread
        };
        let model = MessageWindow {
            msg: init.message,
            thread,
            view,
            account_name: init.account_name,
            account_color: init.account_color,
            allow_remote: init.allow_remote,
            loading: init.loading,
            attachments: init.attachments,
            attachments_available: init.attachments_available,
            attachments_loading: init.attachments_loading,
            attach_list: gtk::Box::new(gtk::Orientation::Vertical, 0),
        };

        let attach_list = model.attach_list.clone();
        let widgets = view_output!();

        // Ctrl+P prints, matching the main window. This window has no menu bar to
        // hang an action off, so the accelerator is wired directly.
        {
            let keys = gtk::EventControllerKey::new();
            let s = sender.clone();
            keys.connect_key_pressed(move |_, keyval, _, state| {
                if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                    // Shift+P is the preview; the keyval arrives capitalised.
                    if keyval == gtk::gdk::Key::P {
                        s.input(MessageWindowInput::PrintPreview);
                        return gtk::glib::Propagation::Stop;
                    }
                    if keyval == gtk::gdk::Key::p {
                        s.input(MessageWindowInput::Print);
                        return gtk::glib::Propagation::Stop;
                    }
                }
                gtk::glib::Propagation::Proceed
            });
            root.add_controller(keys);
        }

        model.render_body();
        model.rebuild_attach_popover(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            MessageWindowInput::Ignore => {}
            MessageWindowInput::SetContentTheme(o) => {
                self.view.emit(MessageViewInput::SetContentTheme(o));
            }
            MessageWindowInput::SetReaderStyle(style) => {
                self.view.emit(MessageViewInput::SetReaderStyle(style));
            }
            MessageWindowInput::SetReaderMode(on) => {
                self.view.emit(MessageViewInput::SetReaderMode(on));
            }
            MessageWindowInput::SetReaderSwitchShown(on) => {
                self.view.emit(MessageViewInput::SetReaderSwitchShown(on));
            }
            MessageWindowInput::SetReaderDefault(policy) => {
                self.view.emit(MessageViewInput::SetReaderDefault(policy));
            }
            MessageWindowInput::ReaderMode(on) => {
                let _ = sender.output(MessageWindowOutput::ReaderMode(on));
            }
            MessageWindowInput::Unsubscribe { message, info } => {
                let _ = sender.output(MessageWindowOutput::Unsubscribe { message, info });
            }
            MessageWindowInput::SetUnsubscribed(lists) => {
                self.view.emit(MessageViewInput::SetUnsubscribed(lists));
            }
            MessageWindowInput::InviteAction { message, invite, action } => {
                let _ = sender.output(MessageWindowOutput::InviteAction { message, invite, action });
            }
            MessageWindowInput::SetIdentities(identities) => {
                self.view.emit(MessageViewInput::SetIdentities(identities));
            }
            MessageWindowInput::SetInviteAnswers(answers) => {
                self.view.emit(MessageViewInput::SetInviteAnswers(answers));
            }
            MessageWindowInput::UnsubscribeState { account_id, id, state } => {
                self.view.emit(MessageViewInput::UnsubscribeState { account_id, id, state });
            }
            MessageWindowInput::FacesChanged => {
                self.view.emit(MessageViewInput::FacesChanged);
            }
            MessageWindowInput::SetSenderCheck(check) => {
                // Light the popout's header seal too (#88).
                self.view.emit(MessageViewInput::SenderCheckFor {
                    account_id: self.msg.account_id,
                    id: self.msg.id,
                    check: check.clone(),
                });
                self.view.emit(MessageViewInput::SetSenderCheck(check));
            }
            MessageWindowInput::SetBody { account_id, id, body } => {
                let mine = self.msg.account_id == account_id && self.msg.id == id;
                let member = self
                    .thread
                    .iter_mut()
                    .find(|t| t.account_id == account_id && t.id == id);
                if !mine && member.is_none() {
                    return;
                }
                if let Some(t) = member {
                    t.body = body.clone();
                }
                if mine {
                    self.msg.body = body;
                    self.loading = false;
                }
                self.render_body();
            }
            MessageWindowInput::SetStarred(starred) => {
                self.msg.starred = starred;
            }
            MessageWindowInput::SetKeywords(keywords) => {
                self.msg.keywords = keywords.clone();
                for m in self.thread.iter_mut() {
                    if m.account_id == self.msg.account_id && m.id == self.msg.id {
                        m.keywords = keywords.clone();
                    }
                }
                self.view.emit(MessageViewInput::SetCardKeywords {
                    account_id: self.msg.account_id,
                    id: self.msg.id,
                    keywords,
                });
            }
            MessageWindowInput::SetTags(tags) => {
                self.view.emit(MessageViewInput::SetTags(tags));
            }
            MessageWindowInput::SetAttachments(items) => {
                self.attachments = items;
                self.attachments_available = false;
                self.attachments_loading = false;
                self.rebuild_attach_popover(&sender);
            }
            MessageWindowInput::AttachmentsPending => {
                self.attachments_available = true;
                self.attachments_loading = false;
            }
            MessageWindowInput::Reply => self.emit_action(RowAction::Reply, &sender),
            MessageWindowInput::ReplyAll => self.emit_action(RowAction::ReplyAll, &sender),
            MessageWindowInput::Forward => self.emit_action(RowAction::Forward, &sender),
            MessageWindowInput::ToggleStar => self.emit_action(RowAction::ToggleStar, &sender),
            MessageWindowInput::ViewSource => self.emit_action(RowAction::ViewSource, &sender),
            // Moving the message away — let the app handle it, then close.
            MessageWindowInput::Print => self.view.emit(MessageViewInput::Print),

            MessageWindowInput::PrintPreview => self.view.emit(MessageViewInput::PrintPreview),

            MessageWindowInput::Delete => {
                self.emit_action(RowAction::Delete, &sender);
                root.close();
            }
            MessageWindowInput::Archive => {
                self.emit_action(RowAction::Archive, &sender);
                root.close();
            }
            MessageWindowInput::Spam => {
                self.emit_action(RowAction::Spam, &sender);
                root.close();
            }
            MessageWindowInput::AddToContacts => {
                let _ = sender.output(MessageWindowOutput::AddToContacts {
                    name: self.msg.from_name.clone(),
                    email: self.msg.from_addr.clone(),
                });
            }
            MessageWindowInput::LoadAttachmentsNow => {
                self.attachments_available = false;
                self.attachments_loading = true;
                let _ = sender.output(MessageWindowOutput::LoadAttachments(Box::new(self.msg.clone())));
            }
            MessageWindowInput::OpenAttachment(i) => {
                if let Some(att) = self.attachments.get(i) {
                    let _ = sender.output(MessageWindowOutput::OpenAttachment(Box::new(att.clone())));
                }
            }
            MessageWindowInput::SaveAllAttachments => {
                let _ = sender.output(MessageWindowOutput::SaveAllAttachments(self.attachments.clone()));
            }
            MessageWindowInput::AllowSender(addr) => {
                let _ = sender.output(MessageWindowOutput::AllowSender(addr));
            }
            MessageWindowInput::ReloadBody(m) => {
                let _ = sender.output(MessageWindowOutput::ReloadBody(m));
            }
            MessageWindowInput::Notice(text) => {
                let _ = sender.output(MessageWindowOutput::Notice(text));
            }
            MessageWindowInput::CardAction { action, message } => {
                let _ = sender.output(MessageWindowOutput::Action { action, message });
            }
            MessageWindowInput::AddContactAddr(addr) => {
                let _ = sender.output(MessageWindowOutput::AddToContacts {
                    name: String::new(),
                    email: addr,
                });
            }
            MessageWindowInput::ContactFor(m) => {
                let _ = sender.output(MessageWindowOutput::AddToContacts {
                    name: m.from_name.clone(),
                    email: m.from_addr.clone(),
                });
            }
            MessageWindowInput::ComposeTo(addr) => {
                let _ = sender.output(MessageWindowOutput::ComposeTo(addr));
            }
        }
    }
}

impl MessageWindow {
    fn emit_action(&self, action: RowAction, sender: &ComponentSender<Self>) {
        let _ = sender.output(MessageWindowOutput::Action {
            action,
            message: Box::new(self.msg.clone()),
        });
    }

    fn render_body(&self) {
        self.view.emit(MessageViewInput::Show {
            thread: self.thread.clone(),
            allow_remote: self.allow_remote,
            account_name: self.account_name.clone(),
            account_color: self.account_color.clone(),
            primary: None,
            folder_labels: std::collections::HashMap::new(),
            loading: self.loading,
            // The popout renders what it was handed; no staged reveal.
            instant: true,
        });
    }

    /// Rebuild the attachments popover (a row per attachment + "Save All").
    fn rebuild_attach_popover(&self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.attach_list.first_child() {
            self.attach_list.remove(&child);
        }
        for (i, att) in self.attachments.iter().enumerate() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("attach-row");

            let info = gtk::Box::new(gtk::Orientation::Vertical, 0);
            info.set_hexpand(true);
            let name = gtk::Label::new(Some(&att.name));
            name.set_halign(gtk::Align::Start);
            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name.set_max_width_chars(28);
            let size = gtk::Label::new(Some(&att.human_size()));
            size.set_halign(gtk::Align::Start);
            size.add_css_class("dim-label");
            size.add_css_class("caption");
            info.append(&name);
            info.append(&size);
            row.append(&info);

            let open = gtk::Button::with_label(&i18n("Open"));
            open.add_css_class("flat");
            open.set_valign(gtk::Align::Center);
            let s = sender.input_sender().clone();
            open.connect_clicked(move |_| {
                let _ = s.send(MessageWindowInput::OpenAttachment(i));
            });
            row.append(&open);
            self.attach_list.append(&row);
        }
        if !self.attachments.is_empty() {
            self.attach_list
                .append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let save = gtk::Button::with_label(&i18n("Save All…"));
            save.add_css_class("flat");
            let s = sender.input_sender().clone();
            save.connect_clicked(move |_| {
                let _ = s.send(MessageWindowInput::SaveAllAttachments);
            });
            self.attach_list.append(&save);
        }
    }
}

/// The window/title text for a message (its subject, or a placeholder).
fn title_text(m: &Message) -> String {
    if m.subject.trim().is_empty() {
        "(No subject)".to_string()
    } else {
        m.subject.clone()
    }
}
