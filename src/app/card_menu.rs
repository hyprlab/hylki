//! The right-click menu of a message card in the reader.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::{FolderKind, Message};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::message_actions::{KeysOf, MessageAction};
use crate::ui::message_list::RowAction;
use crate::ui::message_window::MessageWindowInput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::uri_queue::FILE_FOLDER;

/// One entry of a card's menu that does what a list row's action does.
///
/// Through the card path: a reply started from a card belongs in the
/// pane's inline composer, like the card's own buttons; the row path would
/// open a compose window. Every other action falls through to the row
/// behaviour there. A pop-out has no pane, so it takes the row path, as
/// its toolbar does.
fn card_item<'a>(
    m: &'a Message,
    popout: Option<(u32, u32)>,
    sender: &'a ComponentSender<AppModel>,
) -> impl Fn(RowAction, String, &str) -> MenuEntry + 'a {
    move |action: RowAction, label: String, icon: &str| -> MenuEntry {
        let s = sender.input_sender().clone();
        let message = m.clone();
        MenuEntry::new(label, move || {
            let message = Box::new(message.clone());
            let _ = s.send(if popout.is_some() {
                AppMsg::RowAction {
                    action,
                    message,
                    conversation: Vec::new(),
                }
            } else {
                AppMsg::CardAction { action, message }
            });
        })
        .icon(format!("{icon}-symbolic"))
        .keys(MessageAction::row_keys(action, m.unread, keys_of(popout)))
    }
}

/// Whose keys the card menu names: the pop-out's or the main window's.
fn keys_of(popout: Option<(u32, u32)>) -> KeysOf {
    match popout {
        Some(_) => KeysOf::Popout,
        None => KeysOf::Main,
    }
}

/// Reply, Reply All and Forward; a template (#360) is used or edited, never
/// answered.
fn answer_entries(
    item: &impl Fn(RowAction, String, &str) -> MenuEntry,
    kind: Option<FolderKind>,
) -> Vec<MenuEntry> {
    if kind == Some(FolderKind::Templates) {
        vec![
            item(
                RowAction::EditAsNew,
                i18n("Use Template"),
                "mail-message-new",
            ),
            item(
                RowAction::EditTemplate,
                i18n("Edit Template"),
                "document-edit",
            ),
        ]
    } else {
        vec![
            item(RowAction::Reply, i18n("Reply"), "mail-reply-sender"),
            item(RowAction::ReplyAll, i18n("Reply All"), "mail-reply-all"),
            item(RowAction::Forward, i18n("Forward"), "mail-forward"),
        ]
    }
}

/// Star and read state, each offered the other way round from how it is.
fn flag_entries(
    item: &impl Fn(RowAction, String, &str) -> MenuEntry,
    m: &Message,
) -> Vec<MenuEntry> {
    vec![
        if m.starred {
            item(
                RowAction::ToggleStar,
                i18n("Remove Star"),
                "hylki-non-starred",
            )
        } else {
            item(RowAction::ToggleStar, i18n("Star"), "starred")
        },
        if m.unread {
            item(
                RowAction::ToggleRead,
                i18n("Mark as Read"),
                "hylki-mail-read",
            )
        } else {
            item(RowAction::ToggleRead, i18n("Mark as Unread"), "mail-unread")
        },
    ]
}

/// Expand All and Collapse All, for the whole conversation (#326).
fn fold_entries(hit: &crate::ui::message_view::MenuHit) -> Option<Vec<MenuEntry>> {
    let (any_folded, any_open, reader) = hit.folds.clone()?;
    let open = reader.clone();
    Some(vec![
        MenuEntry::new(i18n("Expand All Messages"), move || {
            open.emit(crate::ui::message_view::MessageViewInput::FoldAll(false));
        })
        .icon("pan-down-symbolic")
        .enabled(any_folded),
        MenuEntry::new(i18n("Collapse All Messages"), move || {
            reader.emit(crate::ui::message_view::MessageViewInput::FoldAll(true));
        })
        .icon("pan-up-symbolic")
        .enabled(any_open),
    ])
}

/// The sender's fonts and colors for this message, or back to the reader's
/// (#56): offered only while the reader's own are on.
fn sender_style_entry(hit: &crate::ui::message_view::MenuHit, m: &Message) -> Option<MenuEntry> {
    let (showing, reader) = hit.sender_style.clone()?;
    let (account_id, id) = (m.account_id, m.id);
    let label = if showing {
        i18n("Back to My Fonts and Colors")
    } else {
        i18n("Show Sender's Fonts and Colors")
    };
    Some(
        MenuEntry::new(label, move || {
            reader.emit(
                crate::ui::message_view::MessageViewInput::ToggleSenderStyle { account_id, id },
            );
        })
        .icon("format-text-rich-symbolic"),
    )
}

/// Create Event and Create Task from the text selected in the card (#637):
/// the item starts from the date and times written in it.
fn selection_calendar_entries(
    m: &Message,
    text: &str,
    sender: &ComponentSender<AppModel>,
) -> Vec<MenuEntry> {
    use crate::calendar::ItemKind;
    let entry = |kind: ItemKind, label: String, icon: &str| {
        let (s, message, text) = (sender.input_sender().clone(), m.clone(), text.to_string());
        MenuEntry::new(label, move || {
            let _ = s.send(AppMsg::ToCalendar(
                super::to_calendar::ToCalendar::FromSelection {
                    message: Box::new(message.clone()),
                    kind,
                    text: text.clone(),
                },
            ));
        })
        .icon(icon)
    };
    vec![
        entry(
            ItemKind::Event,
            i18n("Create Event from Selection…"),
            "x-office-calendar-symbolic",
        ),
        entry(
            ItemKind::Task,
            i18n("Create Task from Selection…"),
            "task-due-symbolic",
        ),
    ]
}

/// Create Event and Create Task (from the selected text when there is
/// some), then a filter and a contact from the message.
fn card_create_sections(
    item: &impl Fn(RowAction, String, &str) -> MenuEntry,
    m: &Message,
    hit: &crate::ui::message_view::MenuHit,
    sender: &ComponentSender<AppModel>,
) -> Vec<Vec<MenuEntry>> {
    vec![
        match &hit.selection {
            Some(text) => selection_calendar_entries(m, text, sender),
            None => vec![
                item(
                    RowAction::CreateEvent,
                    i18n("Create Event…"),
                    "x-office-calendar",
                ),
                item(RowAction::CreateTask, i18n("Create Task…"), "task-due"),
            ],
        },
        vec![
            item(
                RowAction::CreateFilter,
                i18n("Create Filter from Message…"),
                "filter-folder",
            ),
            item(
                RowAction::AddContact,
                i18n("Add Sender to Contacts"),
                "contact-new",
            ),
        ],
    ]
}

/// Translate, Show Original or Show Translation, when a service is set up.
fn translate_entry(m: &Message, hit: &crate::ui::message_view::MenuHit) -> Option<MenuEntry> {
    let t = hit.translate.clone()?;
    let (account_id, id) = (m.account_id, m.id);
    Some(
        MenuEntry::new(t.label, move || {
            t.reader
                .emit(crate::ui::message_view::MessageViewInput::Translate { account_id, id });
        })
        .icon("translate-symbolic"),
    )
}

impl AppModel {
    /// The menu a right-click on a reader card opens (the message list
    /// row's menu, for that one message), anchored on the window at (x, y).
    fn show_card_menu(
        &self,
        m: Message,
        x: f64,
        y: f64,
        hit: crate::ui::message_view::MenuHit,
        popout: Option<(u32, u32)>,
        sender: &ComponentSender<Self>,
    ) {
        // A pop-out's menu opens over the pop-out, and acts as its own
        // buttons do; with the window gone there is nothing to anchor to.
        let parent: gtk::Widget = match popout {
            Some(key) => match self.popouts.get(&key) {
                Some(p) => p.window.clone().upcast(),
                None => return,
            },
            None => self.window.clone().upcast(),
        };
        let window = parent.clone().downcast::<gtk::Window>().ok();
        let item = card_item(&m, popout, sender);
        let kind = self.folder_kind(m.account_id, m.folder_id);
        let mut sections = Vec::new();
        let here = crate::ui::message_view::hit_menu_entries(&hit, window);
        if !here.is_empty() {
            sections.push(here);
        }
        sections.push(answer_entries(&item, kind));
        let from_file = m.folder_id == FILE_FOLDER;
        if from_file {
            // A message open from a file (#398) has no server to star,
            // tag, file or delete it on: it can only be copied into one.
            sections.push(vec![self.copy_to_entry(&m, popout, (x, y), sender)]);
        } else {
            let mut flags = flag_entries(&item, &m);
            flags.extend(self.card_tags_entry(&m, sender));
            sections.push(flags);
            sections.push(self.card_place_entries(&item, &m, kind, popout, (x, y), sender));
        }
        // The first page holds what is done to mail every day; the rest
        // waits one click away (#692), or the menu outgrew the window.
        let mut last = Vec::from_iter(translate_entry(&m, &hit));
        last.push(
            MenuEntry::submenu(
                i18n("Create"),
                card_create_sections(&item, &m, &hit, sender),
            )
            .icon("list-add-symbolic"),
        );
        last.push(
            MenuEntry::submenu(i18n("View"), self.card_view_sections(&m, &hit, sender))
                .icon("eye-open-negative-filled-symbolic"),
        );
        let mut edit = Vec::new();
        if kind != Some(FolderKind::Templates) {
            edit.push(item(
                RowAction::EditAsNew,
                i18n("Edit as New Message"),
                "document-edit",
            ));
        }
        if !from_file {
            edit.push(self.copy_to_entry(&m, popout, (x, y), sender));
        }
        let more = vec![
            edit,
            vec![
                item(RowAction::ViewSource, i18n("View Source"), "code"),
                item(
                    RowAction::SaveAsFile,
                    i18n("Save as File…"),
                    "document-save",
                ),
            ],
            self.card_print_entries(popout, sender),
        ];
        last.push(MenuEntry::submenu(i18n("More"), more).icon("view-more-horizontal-symbolic"));
        sections.push(last);
        show_context_menu(&parent, x, y, sections);
    }

    /// How the message shows: its remote content, the sender's fonts and
    /// colors, and the conversation's folds.
    fn card_view_sections(
        &self,
        m: &Message,
        hit: &crate::ui::message_view::MenuHit,
        sender: &ComponentSender<Self>,
    ) -> Vec<Vec<MenuEntry>> {
        let mut content = vec![self.card_remote_entry(m, sender)];
        content.extend(sender_style_entry(hit, m));
        let mut sections = vec![content];
        sections.extend(fold_entries(hit));
        sections
    }

    /// Copy To…, its picker over the window the menu is in.
    fn copy_to_entry(
        &self,
        m: &Message,
        popout: Option<(u32, u32)>,
        (x, y): (f64, f64),
        sender: &ComponentSender<Self>,
    ) -> MenuEntry {
        let s = sender.input_sender().clone();
        let message = m.clone();
        MenuEntry::new(i18n("Copy To…"), move || {
            let _ = s.send(match popout {
                Some(key) => AppMsg::PopoutCopyTo {
                    key,
                    message: Box::new(message.clone()),
                    x,
                    y,
                },
                None => AppMsg::ListCopyTo {
                    messages: vec![message.clone()],
                    offer_whole: false,
                    x,
                    y,
                },
            });
        })
        .icon("edit-copy-symbolic")
    }

    /// The Tags submenu, when there are tags.
    fn card_tags_entry(&self, m: &Message, sender: &ComponentSender<Self>) -> Option<MenuEntry> {
        if self.tags.is_empty() {
            return None;
        }
        let s = sender.input_sender().clone();
        let message = m.clone();
        let entries =
            crate::ui::message_list::tag_menu_entries(&self.tags, m, move |keyword, add| {
                let _ = s.send(AppMsg::SetTag {
                    message: Box::new(message.clone()),
                    keyword,
                    add,
                });
            });
        Some(
            MenuEntry::submenu(i18n("Tags"), vec![entries])
                .icon("tag-outline-symbolic")
                .keys(MessageAction::Tags.keys(KeysOf::Main)),
        )
    }

    /// Where the message goes: Not Spam from Junk, else back to the Inbox
    /// from Trash and Mark as Spam; Move To… (not from a pop-out: the
    /// folder picker opens over the main window), Archive and Delete.
    fn card_place_entries(
        &self,
        item: &impl Fn(RowAction, String, &str) -> MenuEntry,
        m: &Message,
        kind: Option<FolderKind>,
        popout: Option<(u32, u32)>,
        (x, y): (f64, f64),
        sender: &ComponentSender<Self>,
    ) -> Vec<MenuEntry> {
        let in_junk = kind == Some(FolderKind::Junk);
        let restorable = matches!(kind, Some(FolderKind::Trash | FolderKind::Junk));
        let mut acts = Vec::new();
        if in_junk {
            acts.push(item(
                RowAction::NotSpam,
                i18n("Not Spam"),
                "mail-mark-notjunk",
            ));
        } else {
            if restorable {
                acts.push(item(
                    RowAction::MoveToInbox,
                    i18n("Move to Inbox"),
                    "mail-inbox",
                ));
            }
            acts.push(item(
                RowAction::Spam,
                i18n("Mark as Spam"),
                "mail-mark-junk",
            ));
        }
        if popout.is_none() {
            let s = sender.input_sender().clone();
            let message = m.clone();
            acts.push(
                MenuEntry::new(i18n("Move To…"), move || {
                    let _ = s.send(AppMsg::ListMoveTo {
                        messages: vec![message.clone()],
                        offer_whole: false,
                        x,
                        y,
                    });
                })
                .icon("folder-symbolic")
                .keys(MessageAction::MoveTo.keys(KeysOf::Main)),
            );
        }
        acts.push(item(RowAction::Archive, i18n("Archive"), "mail-archive"));
        // Snooze (#527): the card's message alone, as its other actions.
        if let Some(snoozed) = self.snooze_offer(m.account_id, kind) {
            let s = sender.input_sender().clone();
            let message = m.clone();
            let menu = crate::ui::later::snooze_menu(snoozed, move |choice| {
                let _ = s.send(AppMsg::SnoozeMessages {
                    messages: vec![message.clone()],
                    choice,
                });
            });
            acts.push(
                MenuEntry::submenu(i18n("Snooze"), menu)
                    .icon("alarm-symbolic")
                    .keys(MessageAction::Snooze.keys(keys_of(popout))),
            );
        }
        acts.push(item(RowAction::Delete, i18n("Delete"), "user-trash"));
        acts
    }

    /// Remote content, per message. The banner offers the same thing, but
    /// it can be switched off (Settings → Privacy) and then there is
    /// nothing to click, so the choice lives here too, and here it goes
    /// both ways: the banner can only ever let content in.
    fn card_remote_entry(&self, m: &Message, sender: &ComponentSender<Self>) -> MenuEntry {
        let showing = self.remote_allowed(m);
        let s = sender.input_sender().clone();
        let (account_id, id) = (m.account_id, m.id);
        let label = if showing {
            i18n("Block Remote Content")
        } else {
            i18n("Show Remote Content")
        };
        let icon = if showing {
            "security-high"
        } else {
            "image-x-generic"
        };
        MenuEntry::new(label, move || {
            let _ = s.send(AppMsg::SetRemoteContent {
                account_id,
                id,
                show: !showing,
            });
        })
        .icon(format!("{icon}-symbolic"))
    }

    /// Printing prints what the reader shows, the whole conversation in
    /// it, so the reader that was right-clicked is the one asked (#359).
    fn card_print_entries(
        &self,
        popout: Option<(u32, u32)>,
        sender: &ComponentSender<Self>,
    ) -> Vec<MenuEntry> {
        let print = |label: String, preview: bool| -> MenuEntry {
            let s = sender.input_sender().clone();
            let window = popout
                .and_then(|key| self.popouts.get(&key))
                .map(|p| p.controller.sender().clone());
            MenuEntry::new(label, move || match &window {
                Some(w) => w.emit(if preview {
                    MessageWindowInput::PrintPreview
                } else {
                    MessageWindowInput::Print
                }),
                None => {
                    let _ = s.send(if preview {
                        AppMsg::PrintPreview
                    } else {
                        AppMsg::PrintMessage
                    });
                }
            })
        };
        vec![
            print(i18n("Print Preview"), true)
                .icon("printer-symbolic")
                .keys(Some("<Control><Shift>p")),
            print(i18n("Print…"), false).keys(Some("<Control>p")),
        ]
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_card_menu(&self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ReaderTagMenu => {
                if let Some(entries) = self.reader_tag_entries(&sender) {
                    let btn = &self.reader_tag_btn;
                    crate::ui::context_menu::show_context_menu(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        vec![entries],
                    );
                }
            }

            AppMsg::CardMenu {
                message,
                x,
                y,
                hit,
                popout,
            } => {
                self.show_card_menu(*message, x, y, hit, popout, &sender);
            }
            _ => unreachable!("not routed to on_card_menu"),
        }
    }
}
