//! A message popped out into its own top-level window: a standalone reader with
//! the main window's reader toolbar, in the layout Settings gives it, and the
//! same attachment drawer and lightbox.
//!
//! The window owns its own [`MessageView`] and attachment state, but defers the
//! real work (reply, move, load attachments, …) back to the app via outputs so
//! behaviour stays identical to the main window.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{ReaderToolbar, Tag, ToolbarItem};
use crate::models::{Attachment, Message};
use crate::ui::attachment_drawer::{AttachmentDrawer, AttachmentDrawerInput, DrawerInit, DrawerOutput};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::lightbox::{Lightbox, LightboxInput};
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
    /// Each member's files, as far as they are in hand, by (account, id).
    pub attachments: Vec<((u32, u32), Vec<Attachment>)>,
    /// The members whose files are on their way.
    pub attachments_loading: Vec<(u32, u32)>,
    /// The attachment drawer is on (Settings, #213); off, the files show on
    /// the message's card instead, when that is on.
    pub drawer_enabled: bool,
    pub card_attachments: bool,
    /// The reader toolbar's layout, as the main window has it.
    pub toolbar: ReaderToolbar,
    /// The message is in Junk (Spam reads Not Spam), or in Junk or Trash
    /// (Move to Inbox is offered).
    pub in_junk: bool,
    pub restorable: bool,
    /// Message-content theme override (`None` follows the system).
    pub content_dark: Option<bool>,
    /// The reader's own fonts and colors over the senders' (#56).
    pub reader_style: crate::config::ReaderStyle,
    /// Reader View (the main window's header toggle), followed here too.
    pub reader_mode: bool,
    /// Message zoom in percent (Ctrl+ / Ctrl-), followed here too.
    pub zoom: u32,
    /// The zoom Settings starts at, for the chip.
    pub zoom_default: u32,
    /// Whether the Reader View switch is shown at all.
    pub reader_switch: bool,
    /// Whether the Light / Dark Mode switch is shown (PR #386).
    pub theme_switch: bool,
    /// What Reader View does when a message is opened.
    pub reader_default: crate::config::ReaderDefault,
    /// The tags (#71), for the cards' chips.
    pub tags: Vec<crate::config::Tag>,
    /// The OpenPGP chip says its verdict in words (#300).
    pub pgp_labels: bool,
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
    /// Each member's files, in the order they arrived.
    attachments: Vec<((u32, u32), Vec<Attachment>)>,
    /// The members whose files are on their way.
    attachments_loading: std::collections::HashSet<(u32, u32)>,
    drawer_enabled: bool,
    drawer: Controller<AttachmentDrawer>,
    lightbox: Controller<Lightbox>,
    tags: Vec<Tag>,
    toolbar: ReaderToolbar,
    in_junk: bool,
    restorable: bool,
    /// The right group is folded into the ⋯ menu: the window is too narrow
    /// for the whole row.
    collapsed: bool,
    /// The window and what fills it, for keeping the window's minimum
    /// width at its content's (see [`MessageWindow::fit_min_width`]).
    window: adw::Window,
    content: gtk::Overlay,
    bar: Toolbar,
}

/// The header's buttons, one per toolbar item, packed in the saved order.
struct Toolbar {
    header: adw::HeaderBar,
    buttons: Vec<(ToolbarItem, gtk::Button)>,
    overflow: gtk::Button,
    spinner: gtk::Spinner,
    breakpoint: adw::Breakpoint,
}

#[derive(Debug)]
pub enum MessageWindowInput {
    /// Print this message (Ctrl+P), the same as in the main window.
    Print,
    /// Preview it as it will print (Ctrl+Shift+P).
    PrintPreview,
    /// No-op (unreachable output mapping).
    Ignore,
    /// A message body arrived from the server — the popped-out message itself
    /// or, in a conversation window, any member of the thread.
    SetBody { account_id: u32, id: u32, body: String },
    /// The sender-authentication verdict for this message.
    SetSenderCheck(Box<crate::models::SenderCheck>),
    /// The OpenPGP chip's words on or off (#300).
    SetPgpLabels(bool),
    /// The Settings switch for the "Check this sender" banner changed.
    SetSpoofBannerShown(bool),
    /// Reflect a star toggle that happened elsewhere (or came back from the app).
    SetStarred(bool),
    /// The message's keywords changed (#71).
    SetKeywords(Vec<String>),
    /// The tag definitions changed.
    SetTags(Vec<crate::config::Tag>),
    /// A member's downloaded attachments are now available.
    SetAttachments { account_id: u32, id: u32, items: Vec<Attachment> },
    /// A member's attachments exist but were not on disk: fetch them.
    AttachmentsPending { account_id: u32, id: u32 },
    /// Reflect a read/unread change that happened elsewhere.
    SetUnread(bool),
    /// Settings changed the reader toolbar's layout.
    SetToolbar(ReaderToolbar),
    SetAttachmentDrawer(bool),
    SetCardAttachmentsShown(bool),
    /// The window crossed the width the whole toolbar needs.
    SetCollapsed(bool),
    /// Hand this to the attachment drawer (a deletion's progress, #289).
    Drawer(AttachmentDrawerInput),
    /// Show the lightbox over these previewable attachments.
    Lightbox { items: Vec<Attachment>, start: usize },
    /// A card's attachment row: open or save that member's file at `index`.
    CardAttachment { account_id: u32, id: u32, index: usize, save: bool },
    /// The drawer's Delete from Server.
    DeleteAttachment(Attachment),
    /// The drawer's Show in Message.
    ShowAttachment(Attachment),
    /// Update the message-content theme (`None` follows the system).
    SetContentTheme(Option<bool>),
    /// The reader's own fonts and colors changed (#56).
    SetReaderStyle(crate::config::ReaderStyle),
    SetReaderMode(bool),
    SetZoom(u32),
    SetZoomDefault(u32),
    /// The zoom chip was clicked in this window.
    ZoomReset,
    SetReaderSwitchShown(bool),
    SetThemeSwitchShown(bool),
    SetReaderDefault(crate::config::ReaderDefault),
    /// This window's own Reader View switch was flipped. It changes this
    /// window alone: the main window and the saved choice are left as they
    /// were.
    ReaderMode(bool),
    /// What one of the user's own mailboxes shows changed (#189).
    FacesChanged,
    // ---- toolbar actions ----
    Reply,
    ReplyAll,
    Forward,
    ToggleStar,
    ToggleRead,
    Delete,
    Archive,
    /// Spam, or Not Spam in Junk.
    Spam,
    MoveToInbox,
    /// The folder picker, under the Move To button (or the ⋯).
    MoveTo,
    /// The folder picker at a window point (a card's Move to…).
    MoveToAt { message: Box<Message>, x: f64, y: f64 },
    TagMenu,
    OverflowMenu,
    Find,
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
    /// A right-click in the message: its menu, at this window's point (x, y).
    CardMenu { message: Box<Message>, x: f64, y: f64, hit: crate::ui::message_view::MenuHit },
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
    /// The zoom chip was clicked in this window: back to the default.
    ZoomReset,
    /// A per-message action handled exactly like a list/context-menu action.
    Action { action: RowAction, message: Box<Message> },
    /// Add this sender to Contacts.
    AddToContacts { name: String, email: String },
    /// Download this message's attachments from the server.
    LoadAttachments(Box<Message>),
    /// The folder picker for this message, at window point (x, y).
    MoveTo { message: Box<Message>, x: f64, y: f64 },
    /// Put a tag on this message, or take it off (#71).
    SetTag { message: Box<Message>, keyword: String, add: bool },
    /// Take this attachment out of the message on the server (#289).
    DeleteAttachment { message: Box<Message>, attachment: Box<Attachment> },
    /// Persist a remote-content allowlist entry.
    /// Fetch a message's body again (its OpenPGP verdict changed, #133).
    ReloadBody(Box<Message>),
    /// A toast for the main window.
    Notice(String),
    AllowSender(String),
    /// An email address in a card header was clicked — compose to it.
    ComposeTo(String),
    /// A right-click in the message: the app shows its menu over this window.
    CardMenu { message: Box<Message>, x: f64, y: f64, hit: crate::ui::message_view::MenuHit },
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
                // The main window's reader toolbar: its buttons are packed
                // in `init`, in the order Settings gives them.
                #[name = "header"]
                add_top_bar = &adw::HeaderBar {
                    add_css_class: "flat",
                    add_css_class: "reader-toolbar",
                    #[wrap(Some)]
                    set_title_widget = &gtk::Label {
                        set_label: "",
                    },
                },
                // The drawer holds the reader above its own footer; the
                // lightbox covers both.
                #[wrap(Some)]
                #[name = "content"]
                set_content = &gtk::Overlay {
                    set_child: Some(model.drawer.widget()),
                    add_overlay: model.lightbox.widget(),
                },
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
                MessageViewOutput::CardMenu { message, x, y, hit } => {
                    MessageWindowInput::CardMenu { message, x, y, hit }
                }
                MessageViewOutput::CardMoveTo { message, x, y } => {
                    MessageWindowInput::MoveToAt { message, x, y }
                }
                MessageViewOutput::SelectCards(_) => MessageWindowInput::Ignore,
                MessageViewOutput::AttachmentAction { account_id, id, index, save } => {
                    MessageWindowInput::CardAttachment { account_id, id, index, save }
                }
                // The pop-out keeps its own view's state; the main window's
                // per-message record is not its to write.
                MessageViewOutput::SetRemote { .. } => MessageWindowInput::Ignore,
                MessageViewOutput::ComposeTo(addr) => MessageWindowInput::ComposeTo(addr),
                MessageViewOutput::ReloadBody(m) => MessageWindowInput::ReloadBody(m),
                MessageViewOutput::Notice(text) => MessageWindowInput::Notice(text),
                MessageViewOutput::ReaderMode(on) => MessageWindowInput::ReaderMode(on),
                MessageViewOutput::ZoomReset => MessageWindowInput::ZoomReset,
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
        view.emit(MessageViewInput::SetZoomDefault(init.zoom_default));
        view.emit(MessageViewInput::SetZoom(init.zoom));
        view.emit(MessageViewInput::SetReaderSwitchShown(init.reader_switch));
        view.emit(MessageViewInput::SetThemeSwitchShown(init.theme_switch));
        view.emit(MessageViewInput::SetPgpLabels(init.pgp_labels));
        view.emit(MessageViewInput::SetReaderDefault(init.reader_default));
        view.emit(MessageViewInput::SetTags(init.tags.clone()));
        view.emit(MessageViewInput::SetAttachmentDrawer(init.drawer_enabled));
        view.emit(MessageViewInput::SetCardAttachmentsShown(init.card_attachments));

        // The drawer docks beneath the reader, as in the main window.
        let drawer = AttachmentDrawer::builder()
            .launch(DrawerInit {
                state: crate::config::load_drawer_state(),
                reader: view.widget().clone().upcast(),
            })
            .forward(sender.input_sender(), |out| match out {
                DrawerOutput::ShowLightbox { items, start } => MessageWindowInput::Lightbox { items, start },
                DrawerOutput::ShowInMessage(att) => MessageWindowInput::ShowAttachment(att),
                DrawerOutput::DeleteFromServer(att) => MessageWindowInput::DeleteAttachment(att),
            });
        let lightbox = Lightbox::builder().launch(()).detach();

        let thread = if init.thread.is_empty() {
            vec![init.message.clone()]
        } else {
            init.thread
        };
        let mut model = MessageWindow {
            msg: init.message,
            thread,
            view,
            account_name: init.account_name,
            account_color: init.account_color,
            allow_remote: init.allow_remote,
            loading: init.loading,
            attachments: init.attachments,
            attachments_loading: init.attachments_loading.into_iter().collect(),
            drawer_enabled: init.drawer_enabled,
            drawer,
            lightbox,
            tags: init.tags,
            toolbar: init.toolbar,
            in_junk: init.in_junk,
            restorable: init.restorable,
            collapsed: false,
            window: root.clone(),
            content: gtk::Overlay::new(),
            bar: Toolbar {
                header: adw::HeaderBar::new(),
                buttons: Vec::new(),
                overflow: gtk::Button::new(),
                spinner: gtk::Spinner::new(),
                breakpoint: adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                    adw::BreakpointConditionLengthType::MaxWidth,
                    0.0,
                    adw::LengthUnit::Px,
                )),
            },
        };

        let widgets = view_output!();
        model.bar = build_toolbar(&widgets.header, &sender);
        model.content = widgets.content.clone();
        {
            let s = sender.input_sender().clone();
            model.bar.breakpoint.connect_apply(move |_| {
                let _ = s.send(MessageWindowInput::SetCollapsed(true));
            });
            let s = sender.input_sender().clone();
            model.bar.breakpoint.connect_unapply(move |_| {
                let _ = s.send(MessageWindowInput::SetCollapsed(false));
            });
            root.add_breakpoint(model.bar.breakpoint.clone());
        }
        model.relayout_toolbar();
        model.sync_toolbar();

        // Ctrl+P prints and Ctrl+F finds, matching the main window. This
        // window has no menu bar to hang an action off, so the accelerators
        // are wired directly.
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
                    if keyval == gtk::gdk::Key::f {
                        s.input(MessageWindowInput::Find);
                        return gtk::glib::Propagation::Stop;
                    }
                }
                gtk::glib::Propagation::Proceed
            });
            root.add_controller(keys);
        }

        model.render_body();
        model.sync_attachments();

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
            MessageWindowInput::SetZoom(percent) => {
                self.view.emit(MessageViewInput::SetZoom(percent));
            }
            MessageWindowInput::SetZoomDefault(percent) => {
                self.view.emit(MessageViewInput::SetZoomDefault(percent));
            }
            MessageWindowInput::ZoomReset => {
                let _ = sender.output(MessageWindowOutput::ZoomReset);
            }
            MessageWindowInput::SetReaderMode(on) => {
                self.view.emit(MessageViewInput::SetReaderMode(on));
            }
            MessageWindowInput::SetReaderSwitchShown(on) => {
                self.view.emit(MessageViewInput::SetReaderSwitchShown(on));
            }
            MessageWindowInput::SetThemeSwitchShown(on) => {
                self.view.emit(MessageViewInput::SetThemeSwitchShown(on));
            }
            MessageWindowInput::SetReaderDefault(policy) => {
                self.view.emit(MessageViewInput::SetReaderDefault(policy));
            }
            MessageWindowInput::ReaderMode(on) => {
                self.view.emit(MessageViewInput::SetReaderMode(on));
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
            MessageWindowInput::SetSpoofBannerShown(show) => {
                self.view.emit(MessageViewInput::SetSpoofBannerShown(show));
            }
            MessageWindowInput::SetPgpLabels(on) => self.view.emit(MessageViewInput::SetPgpLabels(on)),
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
                self.sync_toolbar();
            }
            MessageWindowInput::SetUnread(unread) => {
                self.msg.unread = unread;
                self.sync_toolbar();
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
                self.tags = tags.clone();
                self.view.emit(MessageViewInput::SetTags(tags));
                self.sync_toolbar();
            }
            MessageWindowInput::SetAttachments { account_id, id, items } => {
                let key = (account_id, id);
                self.attachments.retain(|(k, _)| *k != key);
                self.attachments.push((key, items));
                self.attachments_loading.remove(&key);
                self.sync_attachments();
                self.sync_toolbar();
            }
            MessageWindowInput::AttachmentsPending { account_id, id } => {
                // Opening the message was the request: fetch them, as the
                // main reader does, rather than wait for a click.
                let Some(m) = self.member(account_id, id).cloned() else { return };
                self.attachments_loading.insert((account_id, id));
                let _ = sender.output(MessageWindowOutput::LoadAttachments(Box::new(m)));
                self.sync_toolbar();
            }
            MessageWindowInput::SetToolbar(layout) => {
                if self.toolbar != layout {
                    self.toolbar = layout;
                    self.relayout_toolbar();
                    self.sync_toolbar();
                }
            }
            MessageWindowInput::SetAttachmentDrawer(on) => {
                self.drawer_enabled = on;
                self.view.emit(MessageViewInput::SetAttachmentDrawer(on));
                self.sync_attachments();
            }
            MessageWindowInput::SetCardAttachmentsShown(on) => {
                self.view.emit(MessageViewInput::SetCardAttachmentsShown(on));
            }
            MessageWindowInput::SetCollapsed(on) => {
                self.collapsed = on;
                self.sync_toolbar();
            }
            MessageWindowInput::Drawer(input) => self.drawer.emit(input),
            MessageWindowInput::Lightbox { items, start } => {
                self.lightbox.emit(LightboxInput::Show { items, start });
            }
            MessageWindowInput::CardAttachment { account_id, id, index, save } => {
                let Some(files) = self.attachments.iter().find(|(k, _)| *k == (account_id, id)).map(|(_, f)| f)
                else {
                    return;
                };
                let Some(att) = files.get(index).cloned() else { return };
                if save {
                    save_attachment(att, root);
                } else if crate::ui::attachment_drawer::previewable(&att) {
                    // The lightbox pages through this message's previewable
                    // files, starting at the one clicked.
                    let previewable = |a: &&Attachment| crate::ui::attachment_drawer::previewable(a);
                    let items: Vec<Attachment> = files.iter().filter(previewable).cloned().collect();
                    let start = files[..=index].iter().filter(previewable).count().saturating_sub(1);
                    self.lightbox.emit(LightboxInput::Show { items, start });
                } else {
                    crate::ui::attachments_gallery::open_bytes(&att.name, &att.data, Some(root.upcast_ref()));
                }
            }
            MessageWindowInput::DeleteAttachment(att) => {
                let Some(message) = self.owner(&att).cloned() else { return };
                let _ = sender.output(MessageWindowOutput::DeleteAttachment {
                    message: Box::new(message),
                    attachment: Box::new(att),
                });
            }
            MessageWindowInput::ShowAttachment(att) => {
                if let Some(m) = self.owner(&att) {
                    self.view.emit(MessageViewInput::ScrollToAttachments { account_id: m.account_id, id: m.id });
                }
            }
            MessageWindowInput::Reply => self.emit_action(RowAction::Reply, &sender),
            MessageWindowInput::ReplyAll => self.emit_action(RowAction::ReplyAll, &sender),
            MessageWindowInput::Forward => self.emit_action(RowAction::Forward, &sender),
            MessageWindowInput::ToggleStar => self.emit_action(RowAction::ToggleStar, &sender),
            MessageWindowInput::ToggleRead => {
                self.emit_action(RowAction::ToggleRead, &sender);
                // The app tells the window back too (SetUnread); flipping it
                // here keeps a quick second click from repeating the first.
                self.msg.unread = !self.msg.unread;
                self.sync_toolbar();
            }
            MessageWindowInput::Print => self.view.emit(MessageViewInput::Print),
            MessageWindowInput::PrintPreview => self.view.emit(MessageViewInput::PrintPreview),
            MessageWindowInput::Find => self.view.emit(MessageViewInput::OpenFind),
            // Moving the message away: let the app handle it, then close.
            MessageWindowInput::Delete => {
                self.emit_action(RowAction::Delete, &sender);
                root.close();
            }
            MessageWindowInput::Archive => {
                self.emit_action(RowAction::Archive, &sender);
                root.close();
            }
            MessageWindowInput::Spam => {
                let action = if self.in_junk { RowAction::NotSpam } else { RowAction::Spam };
                self.emit_action(action, &sender);
                root.close();
            }
            MessageWindowInput::MoveToInbox => {
                self.emit_action(RowAction::MoveToInbox, &sender);
                root.close();
            }
            MessageWindowInput::MoveTo => {
                // Under the Move To button, or under the ⋯ that stands in
                // for it while the toolbar is folded.
                let button = self.button(ToolbarItem::MoveTo).filter(|b| b.is_mapped()).unwrap_or(&self.bar.overflow);
                let Some(point) = button.compute_point(root, &gtk::graphene::Point::new(
                    button.width() as f32 / 2.0,
                    button.height() as f32,
                )) else {
                    return;
                };
                let _ = sender.output(MessageWindowOutput::MoveTo {
                    message: Box::new(self.msg.clone()),
                    x: point.x().into(),
                    y: point.y().into(),
                });
            }
            MessageWindowInput::MoveToAt { message, x, y } => {
                let _ = sender.output(MessageWindowOutput::MoveTo { message, x, y });
            }
            MessageWindowInput::TagMenu => self.show_tag_menu(&sender),
            MessageWindowInput::OverflowMenu => self.show_overflow_menu(&sender),
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
            MessageWindowInput::CardMenu { message, x, y, hit } => {
                let _ = sender.output(MessageWindowOutput::CardMenu { message, x, y, hit });
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
            sent: Default::default(),
            loading: self.loading,
            // The popout renders what it was handed; no staged reveal.
            instant: true,
        });
    }

    /// A member of the window's conversation (or the lone message).
    fn member(&self, account_id: u32, id: u32) -> Option<&Message> {
        self.thread.iter().find(|m| m.account_id == account_id && m.id == id)
    }

    /// The message a file of the drawer came with: the first carrying one
    /// of that name and size, since the drawer shows such a pair once.
    fn owner(&self, att: &Attachment) -> Option<&Message> {
        let (key, _) = self.attachments.iter().find(|(_, files)| {
            files.iter().any(|a| a.name == att.name && a.data.len() == att.data.len())
        })?;
        self.member(key.0, key.1)
    }

    /// The files on the drawer and on each card. The drawer spans the whole
    /// conversation, as the main window's does; a reply pulled in from Sent
    /// can repeat what it was sent with, so a (name, size) pair shows once.
    fn sync_attachments(&self) {
        let mut seen = std::collections::HashSet::new();
        let mut merged = Vec::new();
        let mut rows = std::collections::HashMap::new();
        for m in &self.thread {
            let key = (m.account_id, m.id);
            let Some((_, files)) = self.attachments.iter().find(|(k, _)| *k == key) else { continue };
            for a in files {
                if seen.insert((a.name.clone(), a.data.len())) {
                    merged.push(a.clone());
                }
            }
            if !files.is_empty() {
                let row = files
                    .iter()
                    .map(|a| crate::ui::message_view::CardAttachment { name: a.name.clone(), size: a.data.len() as u64 })
                    .collect::<Vec<_>>();
                rows.insert(key, row);
            }
        }
        // Switched off (#213), the drawer is simply never given anything:
        // empty hides it, seam and all.
        self.drawer.emit(AttachmentDrawerInput::SetItems(if self.drawer_enabled { merged } else { Vec::new() }));
        self.view.emit(MessageViewInput::SetCardAttachments(rows));
        self.fit_min_width();
    }

    /// With breakpoints a window's minimum width is its own size request,
    /// not its content's, and narrower content is cut off at the right. The
    /// toolbar folds to fit, but the reader and the drawer's header cannot,
    /// so the window is kept at least as wide as they need. Measured once
    /// the drawer has taken in its files.
    fn fit_min_width(&self) {
        let window = self.window.clone();
        let content = self.content.clone();
        gtk::glib::idle_add_local_once(move || {
            let need = content.measure(gtk::Orientation::Horizontal, -1).0;
            window.set_size_request(need.max(360), 294);
        });
    }

    fn button(&self, item: ToolbarItem) -> Option<&gtk::Button> {
        self.bar.buttons.iter().find(|(i, _)| *i == item).map(|(_, b)| b)
    }

    /// Pack the buttons in the saved order, as the main window does: the
    /// left group from the start, the right group from the end (which fills
    /// right to left, so in reverse), then the attachments spinner as the
    /// innermost of the right group. Buttons on neither side stay unpacked.
    /// The fold point is measured afresh for the new row.
    fn relayout_toolbar(&self) {
        let header = &self.bar.header;
        for (_, b) in &self.bar.buttons {
            if b.parent().is_some() {
                header.remove(b);
            }
        }
        if self.bar.spinner.parent().is_some() {
            header.remove(&self.bar.spinner);
        }
        for item in &self.toolbar.left {
            if let Some(b) = self.button(*item) {
                header.pack_start(b);
            }
        }
        for item in self.toolbar.right.iter().rev() {
            if let Some(b) = self.button(*item) {
                header.pack_end(b);
            }
        }
        header.pack_end(&self.bar.spinner);

        // The width the whole row needs: every packed button shown, the
        // folded state's ⋯ hidden, plus a little slack so the fold comes a
        // step ahead of a squeeze. Tags without any tag counts too.
        let shown: Vec<(gtk::Button, bool)> = self
            .bar
            .buttons
            .iter()
            .map(|(_, b)| (b.clone(), b.is_visible()))
            .collect();
        for (b, _) in &shown {
            b.set_visible(b.parent().is_some());
        }
        let overflow = self.bar.overflow.is_visible();
        self.bar.overflow.set_visible(false);
        let need = header.measure(gtk::Orientation::Horizontal, -1).1;
        for (b, visible) in shown {
            b.set_visible(visible);
        }
        self.bar.overflow.set_visible(overflow);
        self.bar.breakpoint.set_condition(Some(&adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            f64::from(need + 24),
            adw::LengthUnit::Px,
        )));
    }

    /// The buttons' state: which show (the right group only while the row
    /// fits), and the ones that say the message's state.
    fn sync_toolbar(&self) {
        for (item, b) in &self.bar.buttons {
            let right = self.toolbar.right.contains(item);
            let mut visible = !(right && self.collapsed);
            if *item == ToolbarItem::Tags {
                visible &= !self.tags.is_empty();
            }
            b.set_visible(visible);
            match item {
                ToolbarItem::Star => {
                    b.set_tooltip_text(Some(&if self.msg.starred { i18n("Remove Star") } else { i18n("Star") }));
                    if self.msg.starred {
                        b.add_css_class("star-active");
                    } else {
                        b.remove_css_class("star-active");
                    }
                }
                // The icon shows the ACTION (read envelope = "mark as
                // read"), matching the menus.
                ToolbarItem::ReadUnread => {
                    b.set_icon_name(if self.msg.unread { "hylki-mail-read-symbolic" } else { "mail-unread-symbolic" });
                    b.set_tooltip_text(Some(&if self.msg.unread { i18n("Mark as Read") } else { i18n("Mark as Unread") }));
                }
                ToolbarItem::Spam => {
                    b.set_icon_name(if self.in_junk { "mail-mark-notjunk-symbolic" } else { "mail-mark-junk-symbolic" });
                    b.set_tooltip_text(Some(&if self.in_junk { i18n("Not Spam") } else { i18n("Mark as Spam") }));
                }
                _ => {}
            }
        }
        self.bar.overflow.set_visible(self.collapsed && !self.toolbar.right.is_empty());
        let loading = !self.attachments_loading.is_empty();
        self.bar.spinner.set_spinning(loading);
        self.bar.spinner.set_visible(loading);
    }

    /// The tags (#71), ticked where the message carries them.
    fn tag_entries(&self, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        let s = sender.output_sender().clone();
        let message = self.msg.clone();
        crate::ui::message_list::tag_menu_entries(&self.tags, &self.msg, move |keyword, add| {
            let _ = s.send(MessageWindowOutput::SetTag { message: Box::new(message.clone()), keyword, add });
        })
    }

    fn show_tag_menu(&self, sender: &ComponentSender<Self>) {
        if self.tags.is_empty() {
            return;
        }
        let Some(b) = self.button(ToolbarItem::Tags) else { return };
        show_context_menu(b, f64::from(b.width() / 2), f64::from(b.height()), vec![self.tag_entries(sender)]);
    }

    /// The folded toolbar's ⋯ menu: the right group, in its own order, with
    /// the main window's labels and icons.
    fn show_overflow_menu(&self, sender: &ComponentSender<Self>) {
        let entry = |label: String, icon: &str, msg: fn() -> MessageWindowInput| {
            let s = sender.input_sender().clone();
            MenuEntry::new(label, move || {
                let _ = s.send(msg());
            })
            .icon(format!("{icon}-symbolic"))
        };
        let mut section = Vec::new();
        for item in &self.toolbar.right {
            match item {
                ToolbarItem::Reply => section.push(entry(i18n("Reply"), "mail-reply-sender", || MessageWindowInput::Reply)),
                ToolbarItem::ReplyAll => {
                    section.push(entry(i18n("Reply All"), "mail-reply-all", || MessageWindowInput::ReplyAll))
                }
                ToolbarItem::Forward => section.push(entry(i18n("Forward"), "mail-forward", || MessageWindowInput::Forward)),
                ToolbarItem::Star => section.push(if self.msg.starred {
                    entry(i18n("Remove Star"), "hylki-non-starred", || MessageWindowInput::ToggleStar)
                } else {
                    entry(i18n("Star"), "starred", || MessageWindowInput::ToggleStar)
                }),
                ToolbarItem::Archive => section.push(entry(i18n("Archive"), "mail-archive", || MessageWindowInput::Archive)),
                ToolbarItem::Delete => section.push(entry(i18n("Delete"), "user-trash", || MessageWindowInput::Delete)),
                ToolbarItem::Spam => section.push(if self.in_junk {
                    entry(i18n("Not Spam"), "mail-mark-notjunk", || MessageWindowInput::Spam)
                } else {
                    entry(i18n("Mark as Spam"), "mail-mark-junk", || MessageWindowInput::Spam)
                }),
                ToolbarItem::ReadUnread => section.push(if self.msg.unread {
                    entry(i18n("Mark as Read"), "hylki-mail-read", || MessageWindowInput::ToggleRead)
                } else {
                    entry(i18n("Mark as Unread"), "mail-unread", || MessageWindowInput::ToggleRead)
                }),
                ToolbarItem::Tags => {
                    if !self.tags.is_empty() {
                        section.push(
                            MenuEntry::submenu(i18n("Tags"), vec![self.tag_entries(sender)]).icon("tag-outline-symbolic"),
                        );
                    }
                }
                ToolbarItem::MoveTo => {
                    if self.restorable {
                        section.push(entry(i18n("Move to Inbox"), "mail-inbox", || MessageWindowInput::MoveToInbox));
                    }
                    section.push(entry(i18n("Move To…"), "folder", || MessageWindowInput::MoveTo));
                }
                ToolbarItem::Find => {
                    section.push(entry(i18n("Find in Message"), "loupe-with-arrow", || MessageWindowInput::Find))
                }
                ToolbarItem::Print => {
                    section.push(entry(i18n("Print Preview"), "printer", || MessageWindowInput::PrintPreview))
                }
            }
        }
        let b = &self.bar.overflow;
        show_context_menu(b, f64::from(b.width() / 2), f64::from(b.height()), vec![section]);
    }
}

/// One flat icon button for each toolbar item, with the main window's icons
/// and tooltips, and the ⋯ and spinner the row also needs.
fn build_toolbar(header: &adw::HeaderBar, sender: &ComponentSender<MessageWindow>) -> Toolbar {
    let button = |icon: &str, tooltip: String, msg: fn() -> MessageWindowInput| {
        let b = gtk::Button::from_icon_name(icon);
        b.set_tooltip_text(Some(&tooltip));
        b.add_css_class("flat");
        let s = sender.input_sender().clone();
        b.connect_clicked(move |_| {
            let _ = s.send(msg());
        });
        b
    };
    let buttons = ToolbarItem::ALL
        .iter()
        .map(|item| {
            let b = match item {
                ToolbarItem::Reply => button("mail-reply-sender-symbolic", i18n("Reply"), || MessageWindowInput::Reply),
                ToolbarItem::ReplyAll => {
                    button("mail-reply-all-symbolic", i18n("Reply All"), || MessageWindowInput::ReplyAll)
                }
                ToolbarItem::Forward => button("mail-forward-symbolic", i18n("Forward"), || MessageWindowInput::Forward),
                // One glyph in both states, as in the main window; the
                // starred state carries color only.
                ToolbarItem::Star => button("hylki-non-starred-symbolic", i18n("Star"), || MessageWindowInput::ToggleStar),
                ToolbarItem::Archive => button("mail-archive-symbolic", i18n("Archive"), || MessageWindowInput::Archive),
                ToolbarItem::Delete => button("user-trash-symbolic", i18n("Delete"), || MessageWindowInput::Delete),
                ToolbarItem::Spam => button("mail-mark-junk-symbolic", i18n("Mark as Spam"), || MessageWindowInput::Spam),
                ToolbarItem::ReadUnread => {
                    button("mail-unread-symbolic", i18n("Mark as Unread"), || MessageWindowInput::ToggleRead)
                }
                ToolbarItem::Tags => button("tag-outline-symbolic", i18n("Tags"), || MessageWindowInput::TagMenu),
                ToolbarItem::MoveTo => button("folder-symbolic", i18n("Move To…"), || MessageWindowInput::MoveTo),
                ToolbarItem::Find => {
                    button("loupe-with-arrow-symbolic", i18n("Find in message (Ctrl+F)"), || MessageWindowInput::Find)
                }
                // The preview, not the print dialog: it shows what will come
                // out and prints from there (#359).
                ToolbarItem::Print => {
                    button("printer-symbolic", i18n("Print Preview (Ctrl+Shift+P)"), || MessageWindowInput::PrintPreview)
                }
            };
            (*item, b)
        })
        .collect();
    // Rightmost, beside the window controls, as in the main window.
    let overflow = button("view-more-horizontal-symbolic", i18n("Actions"), || MessageWindowInput::OverflowMenu);
    overflow.set_visible(false);
    header.pack_end(&overflow);
    let spinner = gtk::Spinner::new();
    spinner.set_valign(gtk::Align::Center);
    spinner.set_tooltip_text(Some(&i18n("Downloading attachments…")));
    Toolbar {
        header: header.clone(),
        buttons,
        overflow,
        spinner,
        breakpoint: adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            0.0,
            adw::LengthUnit::Px,
        )),
    }
}

/// Save one attachment where the user picks.
fn save_attachment(att: Attachment, parent: &adw::Window) {
    let dialog = gtk::FileDialog::builder().initial_name(&att.name).title(&i18n("Save Attachment")).build();
    dialog.save(Some(parent), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res {
            if let Some(path) = file.path() {
                let _ = std::fs::write(path, &att.data);
            }
        }
    });
}

/// The window/title text for a message (its subject, or a placeholder).
fn title_text(m: &Message) -> String {
    if m.subject.trim().is_empty() {
        "(No subject)".to_string()
    } else {
        m.subject.clone()
    }
}
