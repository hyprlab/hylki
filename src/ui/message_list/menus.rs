//! The row's context menu and the bulk bar's menu.

use super::threading::{heads_its_row, thread_slot};
use super::types::{BulkAction, MessageListInput, MessageListOutput};
use super::{tag_menu_entries, MessageList, RowAction};
use crate::i18n::i18n;
use crate::models::Message;
use crate::ui::context_menu::{show_context_menu_popover, MenuEntry};
use crate::ui::message_actions::{KeysOf, MessageAction as A};
use adw::prelude::*;
use relm4::prelude::*;

/// Makes one menu entry that sends the row's action for the clicked message.
type MakeEntry<'a> = dyn Fn(RowAction, &str, &str) -> MenuEntry + 'a;

impl MessageList {
    /// Build and pop up the right-click menu for `msg` at the click location.
    pub(super) fn show_context_menu(
        &self,
        msg: &Message,
        x: f64,
        y: f64,
        sender: &ComponentSender<Self>,
    ) {
        // Each entry carries the same icon as the reader-toolbar button (or
        // row-palette button) for that action, tying the two together.
        let conversation = self.row_conversation(msg);
        let item = |action: RowAction, label: &str, icon: &str| -> MenuEntry {
            let s = sender.clone();
            let m = msg.clone();
            let conversation = conversation.clone();
            MenuEntry::new(label, move || {
                let _ = s.output(MessageListOutput::Action {
                    action,
                    message: Box::new(m.clone()),
                    conversation: conversation.clone(),
                });
            })
            .icon(icon)
            .keys(A::row_keys(action, msg.unread, KeysOf::Main))
        };
        let mut flag_section = vec![self.star_entry(msg, sender, &item)];
        let move_entry = self.move_entry(msg, x, y, sender);
        let copy_entry = self.copy_entry(msg, x, y, sender);
        flag_section.extend(self.read_entry(msg, sender, &item));
        let tag_section = self.tag_section(msg, sender);
        let mut move_section = self.move_section(move_entry, copy_entry, &item);
        if let Some(snoozed) = self.snooze_offer {
            let s = sender.clone();
            let m = msg.clone();
            let conversation = conversation.clone();
            let menu = crate::ui::later::snooze_menu(snoozed, move |choice| {
                let _ = s.output(MessageListOutput::Action {
                    action: RowAction::Snooze(choice),
                    message: Box::new(m.clone()),
                    conversation: conversation.clone(),
                });
            });
            // After Archive, as on the toolbar.
            let at = move_section.len().saturating_sub(1);
            move_section.insert(
                at,
                MenuEntry::submenu(i18n("Snooze"), menu)
                    .icon("alarm-symbolic")
                    .keys(A::Snooze.keys(KeysOf::Main)),
            );
        }
        let sections = vec![
            self.reply_section(&item),
            flag_section,
            tag_section,
            move_section,
            vec![
                item(
                    RowAction::AddContact,
                    &i18n("Add Sender to Contacts"),
                    "contact-new-symbolic",
                ),
                item(
                    RowAction::CreateFilter,
                    &i18n("Create Filter from Message…"),
                    "filter-folder-symbolic",
                ),
            ],
            vec![
                item(RowAction::ViewSource, &i18n("View Source"), "code-symbolic"),
                item(
                    RowAction::SaveAsFile,
                    &i18n("Save as File…"),
                    "document-save-symbolic",
                ),
            ],
        ];

        self.popup_menu(x, y, None, sections);
    }

    /// Reply, Reply All, Forward and Edit as New; in Templates, the
    /// template's two uses instead.
    fn reply_section(&self, item: &MakeEntry<'_>) -> Vec<MenuEntry> {
        if self.in_templates {
            vec![
                item(
                    RowAction::EditAsNew,
                    &i18n("Use Template"),
                    "mail-message-new-symbolic",
                ),
                item(
                    RowAction::EditTemplate,
                    &i18n("Edit Template"),
                    "document-edit-symbolic",
                ),
            ]
        } else {
            vec![
                item(
                    RowAction::Reply,
                    &i18n("Reply"),
                    "mail-reply-sender-symbolic",
                ),
                item(
                    RowAction::ReplyAll,
                    &i18n("Reply All"),
                    "mail-reply-all-symbolic",
                ),
                item(
                    RowAction::Forward,
                    &i18n("Forward"),
                    "mail-forward-symbolic",
                ),
                item(
                    RowAction::EditAsNew,
                    &i18n("Edit as New Message"),
                    "document-edit-symbolic",
                ),
            ]
        }
    }

    /// The star: a conversation row's acts on the whole thread, like its
    /// read toggle.
    fn star_entry(
        &self,
        msg: &Message,
        sender: &ComponentSender<Self>,
        item: &MakeEntry<'_>,
    ) -> MenuEntry {
        let members_for_star = self.thread_members(msg);
        let star_is_head = members_for_star
            .first()
            .is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        if star_is_head && members_for_star.len() > 1 {
            let any = members_for_star.iter().any(|m| m.starred);
            let s = sender.clone();
            let members = members_for_star.clone();
            MenuEntry::new(
                if any {
                    i18n("Remove Stars")
                } else {
                    i18n("Star Conversation")
                },
                move || {
                    let _ = s.output(MessageListOutput::Bulk {
                        action: if any {
                            BulkAction::Unflag
                        } else {
                            BulkAction::Flag
                        },
                        messages: members.clone(),
                    });
                },
            )
            .icon(if any {
                "hylki-non-starred-symbolic"
            } else {
                "starred-symbolic"
            })
            .keys(A::Star.keys(KeysOf::Main))
        } else if msg.starred {
            item(
                RowAction::ToggleStar,
                &i18n("Remove Star"),
                "hylki-non-starred-symbolic",
            )
        } else {
            item(RowAction::ToggleStar, &i18n("Star"), "starred-symbolic")
        }
    }

    /// Move To…: the clicked message, then (for a conversation row) the
    /// rest of its members, at the click's point in the window so the
    /// app can anchor the picker there.
    fn move_entry(
        &self,
        msg: &Message,
        x: f64,
        y: f64,
        sender: &ComponentSender<Self>,
    ) -> MenuEntry {
        let (messages, offer_whole) = self.move_to_messages(msg);
        let (wx, wy) = self.window_point(x, y);
        let s = sender.clone();
        MenuEntry::new(i18n("Move To…"), move || {
            let _ = s.output(MessageListOutput::MoveTo {
                messages: messages.clone(),
                offer_whole,
                x: wx,
                y: wy,
            });
        })
        .icon("folder-symbolic")
        .keys(A::MoveTo.keys(KeysOf::Main))
    }

    /// Copy To… (#552): what Move To… offers, for a copy.
    fn copy_entry(
        &self,
        msg: &Message,
        x: f64,
        y: f64,
        sender: &ComponentSender<Self>,
    ) -> MenuEntry {
        let (messages, offer_whole) = self.move_to_messages(msg);
        let (wx, wy) = self.window_point(x, y);
        let s = sender.clone();
        MenuEntry::new(i18n("Copy To…"), move || {
            let _ = s.output(MessageListOutput::CopyTo {
                messages: messages.clone(),
                offer_whole,
                x: wx,
                y: wy,
            });
        })
        .icon("edit-copy-symbolic")
    }

    /// The read toggle, if the row has one. A conversation row acts on the
    /// whole thread: its read entry marks every member, through the same
    /// bulk path as a multi-select, and the singular toggle is dropped (the
    /// row isn't a singular message). Expanded replies keep the singular
    /// toggle. Labels follow state, like the singular one: any unread member
    /// reads as an unread conversation.
    fn read_entry(
        &self,
        msg: &Message,
        sender: &ComponentSender<Self>,
        item: &MakeEntry<'_>,
    ) -> Option<MenuEntry> {
        let members = self.thread_members(msg);
        let is_thread_head = members
            .first()
            .is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        if self.in_drafts {
            // A draft is neither read nor unread: no toggle to offer.
            None
        } else if is_thread_head {
            let any_unread = members.iter().any(|m| m.unread);
            let s = sender.clone();
            Some(
                MenuEntry::new(
                    if any_unread {
                        i18n("Mark All as Read")
                    } else {
                        i18n("Mark All as Unread")
                    },
                    move || {
                        let _ = s.output(MessageListOutput::Bulk {
                            action: if any_unread {
                                BulkAction::MarkRead
                            } else {
                                BulkAction::MarkUnread
                            },
                            messages: members.clone(),
                        });
                    },
                )
                .icon(if any_unread {
                    "hylki-mail-read-symbolic"
                } else {
                    "mail-unread-symbolic"
                })
                .keys(
                    if any_unread {
                        A::MarkRead
                    } else {
                        A::MarkUnread
                    }
                    .keys(KeysOf::Main),
                ),
            )
        } else if msg.unread {
            Some(item(
                RowAction::ToggleRead,
                &i18n("Mark as Read"),
                "hylki-mail-read-symbolic",
            ))
        } else {
            Some(item(
                RowAction::ToggleRead,
                &i18n("Mark as Unread"),
                "mail-unread-symbolic",
            ))
        }
    }

    /// Tags (#71): one toggle per tag, a filled swatch where the message
    /// carries it, behind a "Tags" submenu so a long list never makes this
    /// menu too tall. Absent until a tag exists.
    fn tag_section(&self, msg: &Message, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        let tags = self.shared.tags.borrow().clone();
        let s = sender.clone();
        let m = msg.clone();
        let entries = tag_menu_entries(&tags, msg, move |keyword, add| {
            let _ = s.output(MessageListOutput::SetTag {
                message: Box::new(m.clone()),
                keyword,
                add,
            });
        });
        if entries.is_empty() {
            Vec::new()
        } else {
            vec![MenuEntry::submenu(i18n("Tags"), vec![entries])
                .icon("tag-outline-symbolic")
                .keys(A::Tags.keys(KeysOf::Main))]
        }
    }

    /// Spam (or Not Spam in Junk), Move to Inbox where it applies, Move
    /// To…, Archive and Delete.
    fn move_section(
        &self,
        move_entry: MenuEntry,
        copy_entry: MenuEntry,
        item: &MakeEntry<'_>,
    ) -> Vec<MenuEntry> {
        let mut section = Vec::new();
        // In Junk the way back is "Not Spam" (#168): the server is
        // told, and the message returns to the Inbox. In Trash it
        // is a plain move, with spam still on offer.
        if self.in_junk {
            section.push(item(
                RowAction::NotSpam,
                &i18n("Not Spam"),
                "mail-mark-notjunk-symbolic",
            ));
        } else {
            if self.restorable {
                section.push(item(
                    RowAction::MoveToInbox,
                    &i18n("Move to Inbox"),
                    "mail-inbox-symbolic",
                ));
            }
            section.push(item(
                RowAction::Spam,
                &i18n("Mark as Spam"),
                "mail-mark-junk-symbolic",
            ));
        }
        section.push(move_entry);
        section.push(copy_entry);
        section.push(item(
            RowAction::Archive,
            &i18n("Archive"),
            "mail-archive-symbolic",
        ));
        section.push(item(
            RowAction::Delete,
            &i18n("Delete"),
            "user-trash-symbolic",
        ));
        section
    }

    /// Open a menu on the list, keeping the list where it was scrolled. The
    /// popover hands focus back to the list view as it closes, and the list
    /// view then scrolls to the row its focus starts from, the first one:
    /// every right-click menu, tagging included, sent the list to the top.
    fn popup_menu(&self, x: f64, y: f64, header: Option<&str>, sections: Vec<Vec<MenuEntry>>) {
        let before = self
            .list_view
            .root()
            .and_then(|r| gtk::prelude::RootExt::focus(&r));
        let popover = show_context_menu_popover(&self.list_view, x, y, header, sections);
        let Some(scroller) = self.scroller.clone() else {
            return;
        };
        let adj = scroller.vadjustment();
        let pos = adj.value();
        popover.connect_closed(move |_| {
            // Focus goes back where it was, and the list to where it was
            // scrolled, once GTK has done its own focus hand-back.
            let (adj, before) = (adj.clone(), before.clone());
            gtk::glib::idle_add_local_once(move || {
                if let Some(w) = before.filter(|w| w.root().is_some() && w.is_mapped()) {
                    w.grab_focus();
                }
                adj.set_value(pos);
            });
        });
    }

    /// The messages of the selected rows.
    pub(super) fn selected_messages(&self) -> Vec<Message> {
        self.selected_positions()
            .into_iter()
            .filter_map(|p| self.shown.get(p).map(|m| Message::clone(m)))
            .collect()
    }

    /// The selected rows' messages, a conversation's row bringing every
    /// message of it this folder holds, each once.
    pub(super) fn selected_conversations(&self) -> Vec<Message> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for m in self.selected_messages() {
            for x in self.conversation_for(&m).0 {
                if seen.insert((x.account_id, x.id)) {
                    out.push(x);
                }
            }
        }
        out
    }

    /// Whether every selected message is starred, so the bulk star takes
    /// the stars off rather than adding them.
    pub(super) fn selection_all_starred(&self) -> bool {
        let positions = self.selected_positions();
        !positions.is_empty()
            && positions
                .iter()
                .all(|&p| self.shown.get(p).is_some_and(|m| m.starred))
    }

    /// Whether any selected message is unread, so the bulk read toggle
    /// marks the selection read rather than unread.
    pub(super) fn selection_any_unread(&self) -> bool {
        // The rows first: asked on every update, and a row's own message
        // usually answers it.
        let positions = self.selected_positions();
        if positions
            .iter()
            .any(|&p| self.shown.get(p).is_some_and(|m| m.unread))
        {
            return true;
        }
        if !self.threading {
            return false;
        }
        // Then the rest of each conversation a row stands for, as
        // `selected_conversations` gathers it, without copying any.
        let threads = self.source_threads();
        let source = self.active_source();
        positions
            .iter()
            .filter_map(|&p| self.shown.get(p))
            .any(|m| {
                let key = (m.account_id, m.id);
                !self.nested.contains_key(&key)
                    && heads_its_row(key, &self.msg_thread, &self.thread_members)
                    && threads
                        .keys
                        .get(&thread_slot(m))
                        .and_then(|k| threads.members.get(k))
                        .is_some_and(|idx| {
                            idx.iter().any(|&i| source.get(i).is_some_and(|x| x.unread))
                        })
            })
    }

    /// One toggle per tag for the whole selection (#313). A tag is ticked
    /// when every selected message carries it: choosing it then takes it
    /// off them all, and otherwise puts it on them all.
    pub(super) fn bulk_tag_entries(&self, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        let messages = self.selected_messages();
        let Some(first) = messages.first() else {
            return Vec::new();
        };
        let tags = self.shared.tags.borrow().clone();
        let mut all = first.clone();
        all.keywords = first.tag_keywords(&tags);
        all.keywords.retain(|k| {
            messages.iter().all(|m| {
                m.tag_keywords(&tags)
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(k))
            })
        });
        let s = sender.clone();
        tag_menu_entries(&tags, &all, move |keyword, add| {
            let _ = s.output(MessageListOutput::SetTagMany {
                messages: messages.clone(),
                keyword,
                add,
            });
        })
    }

    /// Build and pop up the bulk-action menu for the current multi-selection.
    pub(super) fn show_bulk_menu(&self, x: f64, y: f64, sender: &ComponentSender<Self>) {
        let item = |action: BulkAction, label: &str, icon: &str| -> MenuEntry {
            let s = sender.clone();
            let hint = match action {
                BulkAction::MarkRead => Some(A::MarkRead),
                BulkAction::MarkUnread => Some(A::MarkUnread),
                BulkAction::Flag | BulkAction::Unflag => Some(A::Star),
                BulkAction::Archive => Some(A::Archive),
                BulkAction::Spam => Some(A::MarkSpam),
                BulkAction::NotSpam => Some(A::NotSpam),
                BulkAction::Delete => Some(A::Delete),
                _ => None,
            };
            MenuEntry::new(label, move || s.input(MessageListInput::Bulk(action)))
                .icon(icon)
                .keys(hint.and_then(|a| a.keys(KeysOf::Main)))
        };

        let sections = vec![
            {
                let mut section = Vec::new();
                // Drafts are neither read nor unread.
                // One entry, the way the bar's button toggles.
                if !self.in_drafts {
                    section.push(if self.selection_any_unread() {
                        item(
                            BulkAction::MarkRead,
                            &i18n("Mark as Read"),
                            "hylki-mail-read-symbolic",
                        )
                    } else {
                        item(
                            BulkAction::MarkUnread,
                            &i18n("Mark as Unread"),
                            "mail-unread-symbolic",
                        )
                    });
                }
                // The Bulk handler turns this into Unflag for a starred
                // selection.
                section.push(if self.selection_all_starred() {
                    item(
                        BulkAction::Flag,
                        &i18n("Remove Star"),
                        "hylki-non-starred-symbolic",
                    )
                } else {
                    item(BulkAction::Flag, &i18n("Star"), "starred-symbolic")
                });
                let tags = self.bulk_tag_entries(sender);
                if !tags.is_empty() {
                    section.push(
                        MenuEntry::submenu(i18n("Tags"), vec![tags])
                            .icon("tag-outline-symbolic")
                            .keys(A::Tags.keys(KeysOf::Main)),
                    );
                }
                section
            },
            {
                let mut section = Vec::new();
                if self.in_junk {
                    section.push(item(
                        BulkAction::NotSpam,
                        &i18n("Not Spam"),
                        "mail-mark-notjunk-symbolic",
                    ));
                } else {
                    if self.restorable {
                        section.push(item(
                            BulkAction::MoveToInbox,
                            &i18n("Move to Inbox"),
                            "mail-inbox-symbolic",
                        ));
                    }
                    section.push(item(
                        BulkAction::Spam,
                        &i18n("Mark as Spam"),
                        "mail-mark-junk-symbolic",
                    ));
                }
                {
                    // Move To… for the whole selection, conversations whole.
                    let messages = self.selected_conversations();
                    let (wx, wy) = self.window_point(x, y);
                    let s = sender.clone();
                    section.push(
                        MenuEntry::new(i18n("Move To…"), move || {
                            let _ = s.output(MessageListOutput::MoveTo {
                                messages: messages.clone(),
                                offer_whole: false,
                                x: wx,
                                y: wy,
                            });
                        })
                        .icon("folder-symbolic")
                        .keys(A::MoveTo.keys(KeysOf::Main)),
                    );
                    let messages = self.selected_conversations();
                    let s = sender.clone();
                    section.push(
                        MenuEntry::new(i18n("Copy To…"), move || {
                            let _ = s.output(MessageListOutput::CopyTo {
                                messages: messages.clone(),
                                offer_whole: false,
                                x: wx,
                                y: wy,
                            });
                        })
                        .icon("edit-copy-symbolic"),
                    );
                }
                section.push(item(
                    BulkAction::Archive,
                    &i18n("Archive"),
                    "mail-archive-symbolic",
                ));
                if let Some(snoozed) = self.snooze_offer {
                    let messages = self.selected_conversations();
                    let s = sender.clone();
                    let menu = crate::ui::later::snooze_menu(snoozed, move |choice| {
                        let _ = s.output(MessageListOutput::Snooze {
                            messages: messages.clone(),
                            choice,
                        });
                    });
                    section.push(
                        MenuEntry::submenu(i18n("Snooze"), menu)
                            .icon("alarm-symbolic")
                            .keys(A::Snooze.keys(KeysOf::Main)),
                    );
                }
                section.push(item(
                    BulkAction::Delete,
                    &i18n("Delete"),
                    "user-trash-symbolic",
                ));
                section
            },
        ];

        self.popup_menu(
            x,
            y,
            Some(&format!("{} selected", self.selection_count)),
            sections,
        );
    }

    /// What a Move To… on `msg` offers: the message, then — when it heads a
    /// conversation — the rest of its members, with the whole-conversation
    /// switch (#171).
    pub(super) fn move_to_messages(&self, msg: &Message) -> (Vec<Message>, bool) {
        let members = self.thread_members(msg);
        let is_head = members
            .first()
            .is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        let mut messages = vec![msg.clone()];
        let offer_whole = is_head && members.len() > 1;
        if offer_whole {
            messages.extend(
                members
                    .iter()
                    .filter(|m| (m.account_id, m.id) != (msg.account_id, msg.id))
                    .cloned(),
            );
        }
        (messages, offer_whole)
    }
}
