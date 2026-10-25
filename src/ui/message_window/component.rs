//! The pop-out reader as a relm4 component: its window, building it, and
//! `update`.

use super::content::title_text;
use super::keys::key_controller;
use super::toolbar::{build_toolbar, Toolbar};
use super::types::{MessageWindowInit, MessageWindowInput, MessageWindowOutput};
use super::MessageWindow;
use crate::ui::attachment_drawer::{AttachmentDrawer, DrawerInit, DrawerOutput};
use crate::ui::lightbox::{Lightbox, LightboxInput};
use crate::ui::message_list::RowAction;
use crate::ui::message_view::{MessageView, MessageViewInput, MessageViewOutput};
use adw::prelude::*;
use relm4::prelude::*;

#[relm4::component(pub(crate))]
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
            .forward(sender.input_sender(), from_reader);
        // Apply the message-content theme before the first render.
        view.emit(MessageViewInput::SetContentTheme(init.content_dark));
        view.emit(MessageViewInput::SetReaderStyle(init.reader_style.clone()));
        view.emit(MessageViewInput::SetReaderMode(init.reader_mode));
        view.emit(MessageViewInput::SetZoomDefault(init.zoom_default));
        view.emit(MessageViewInput::SetZoom(init.zoom));
        view.emit(MessageViewInput::SetReaderSwitchShown(init.reader_switch));
        view.emit(MessageViewInput::SetThemeSwitchShown(init.theme_switch));
        view.emit(MessageViewInput::SetPgpLabels(init.pgp_labels));
        view.emit(MessageViewInput::SetFindBottom(init.find_bottom));
        view.emit(MessageViewInput::SetReaderDefault(init.reader_default));
        view.emit(MessageViewInput::SetTags(init.tags.clone()));
        view.emit(MessageViewInput::SetAttachmentDrawer(init.drawer_enabled));
        view.emit(MessageViewInput::SetCardAttachmentsShown(
            init.card_attachments,
        ));

        // The drawer docks beneath the reader, as in the main window.
        let drawer = AttachmentDrawer::builder()
            .launch(DrawerInit {
                state: crate::config::load_drawer_state(),
                reader: view.widget().clone().upcast(),
            })
            .forward(sender.input_sender(), |out| match out {
                DrawerOutput::ShowLightbox { items, start } => {
                    MessageWindowInput::Lightbox { items, start }
                }
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
            snooze: init.snooze,
            collapsed: false,
            fold_right: init.toolbar_fold_right,
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
            gmail_keys: init.gmail_keys.clone(),
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

        root.add_controller(key_controller(
            &sender,
            &root,
            init.single_key,
            init.gmail_keys,
        ));
        root.add_controller(crate::ui::close_key());

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
            MessageWindowInput::Zoom(step) => {
                let _ = sender.output(MessageWindowOutput::Zoom(step));
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
            MessageWindowInput::InviteAction {
                message,
                invite,
                action,
            } => self.on_invite_action(message, invite, action, &sender),
            MessageWindowInput::SetIdentities(identities) => {
                self.view.emit(MessageViewInput::SetIdentities(identities));
            }
            MessageWindowInput::SetInviteAnswers(answers) => {
                self.view.emit(MessageViewInput::SetInviteAnswers(answers));
            }
            MessageWindowInput::UnsubscribeState {
                account_id,
                id,
                state,
            } => self.on_unsubscribe_state(account_id, id, state),
            MessageWindowInput::FacesChanged => {
                self.view.emit(MessageViewInput::FacesChanged);
            }
            MessageWindowInput::SetSpoofBannerShown(show) => {
                self.view.emit(MessageViewInput::SetSpoofBannerShown(show));
            }
            MessageWindowInput::SetPgpLabels(on) => {
                self.view.emit(MessageViewInput::SetPgpLabels(on))
            }
            MessageWindowInput::SetFindBottom(on) => {
                self.view.emit(MessageViewInput::SetFindBottom(on))
            }
            MessageWindowInput::SetSenderCheck(check) => self.on_set_sender_check(check, root),
            MessageWindowInput::SetBody {
                account_id,
                id,
                body,
            } => self.on_set_body(account_id, id, body),
            MessageWindowInput::SetStarred(starred) => {
                self.msg.starred = starred;
                self.sync_toolbar();
            }
            MessageWindowInput::SetUnread(unread) => {
                self.msg.unread = unread;
                self.sync_toolbar();
            }
            MessageWindowInput::SetKeywords(keywords, labels) => {
                self.on_set_keywords(keywords, labels)
            }
            MessageWindowInput::SetTags(tags) => {
                self.tags = tags.clone();
                self.view.emit(MessageViewInput::SetTags(tags));
                self.sync_toolbar();
            }
            MessageWindowInput::SetAttachments {
                account_id,
                id,
                items,
            } => self.on_set_attachments(account_id, id, items),
            MessageWindowInput::AttachmentsPending { account_id, id } => {
                self.on_attachments_pending(account_id, id, &sender)
            }
            MessageWindowInput::SetToolbar(layout) => {
                if self.toolbar != layout {
                    self.toolbar = layout;
                    self.relayout_toolbar();
                    self.sync_toolbar();
                }
            }
            MessageWindowInput::SetToolbarFold(on) => {
                self.fold_right = on;
                self.sync_toolbar();
            }
            MessageWindowInput::SetAttachmentDrawer(on) => {
                self.drawer_enabled = on;
                self.view.emit(MessageViewInput::SetAttachmentDrawer(on));
                self.sync_attachments();
            }
            MessageWindowInput::SetCardAttachmentsShown(on) => {
                self.view
                    .emit(MessageViewInput::SetCardAttachmentsShown(on));
            }
            MessageWindowInput::SetCollapsed(on) => {
                self.collapsed = on;
                self.sync_toolbar();
            }
            MessageWindowInput::Drawer(input) => self.drawer.emit(input),
            MessageWindowInput::Lightbox { items, start } => {
                self.lightbox.emit(LightboxInput::Show { items, start });
            }
            MessageWindowInput::CardAttachment {
                account_id,
                id,
                index,
                save,
            } => self.on_card_attachment(account_id, id, index, save, root),
            MessageWindowInput::DeleteAttachment(att) => self.on_delete_attachment(att, &sender),
            MessageWindowInput::ShowAttachment(att) => {
                if let Some(m) = self.owner(&att) {
                    self.view.emit(MessageViewInput::ScrollToAttachments {
                        account_id: m.account_id,
                        id: m.id,
                    });
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
            MessageWindowInput::Spam => self.on_spam(&sender, root),
            MessageWindowInput::MoveToInbox => {
                self.emit_action(RowAction::MoveToInbox, &sender);
                root.close();
            }
            MessageWindowInput::MoveTo => self.on_move_to(&sender, root),
            MessageWindowInput::MoveToAt { message, x, y } => {
                let _ = sender.output(MessageWindowOutput::MoveTo { message, x, y });
            }
            MessageWindowInput::Key(action) => self.run_key(action, &sender, root),
            MessageWindowInput::TagMenu => self.show_tag_menu(&sender),
            MessageWindowInput::SnoozeMenu => self.show_snooze_menu(&sender),
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

/// What the pop-out does with its reader's outputs: most become its own
/// inputs; the ones that only mean something in the main window are
/// ignored.
fn from_reader(out: MessageViewOutput) -> MessageWindowInput {
    match out {
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
        MessageViewOutput::AttachmentAction {
            account_id,
            id,
            index,
            save,
        } => MessageWindowInput::CardAttachment {
            account_id,
            id,
            index,
            save,
        },
        // The pop-out keeps its own view's state; the main window's
        // per-message record is not its to write.
        MessageViewOutput::SetRemote { .. } => MessageWindowInput::Ignore,
        MessageViewOutput::ComposeTo(addr) => MessageWindowInput::ComposeTo(addr),
        MessageViewOutput::ReloadBody(m) => MessageWindowInput::ReloadBody(m),
        MessageViewOutput::Notice(text) => MessageWindowInput::Notice(text),
        MessageViewOutput::ReaderMode(on) => MessageWindowInput::ReaderMode(on),
        MessageViewOutput::Zoom(step) => MessageWindowInput::Zoom(step),
        MessageViewOutput::Unsubscribe { message, info } => {
            MessageWindowInput::Unsubscribe { message, info }
        }
        MessageViewOutput::InviteAction {
            message,
            invite,
            action,
        } => MessageWindowInput::InviteAction {
            message,
            invite,
            action,
        },
        MessageViewOutput::AddContactAddr(addr) => MessageWindowInput::AddContactAddr(addr),
    }
}
