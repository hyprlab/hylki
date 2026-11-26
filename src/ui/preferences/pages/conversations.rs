//! Settings → Conversations: threading and how conversations unfold.

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, Preferences, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct ConversationsPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) threading_row: adw::SwitchRow,
    pub(in crate::ui::preferences) thread_expansion_row: adw::SwitchRow,
    pub(in crate::ui::preferences) threads_expanded_row: adw::SwitchRow,
    pub(in crate::ui::preferences) thread_newest_first_row: adw::SwitchRow,
    pub(in crate::ui::preferences) fold_messages_row: adw::ComboRow,
    pub(in crate::ui::preferences) thread_row_newest_row: adw::SwitchRow,
    pub(in crate::ui::preferences) reply_position_row: adw::ComboRow,
    pub(in crate::ui::preferences) card_attachments_row: adw::SwitchRow,
    pub(in crate::ui::preferences) attachment_drawer_row: adw::SwitchRow,
    pub(in crate::ui::preferences) confirm_thread_delete_row: adw::SwitchRow,
}

impl ConversationsPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Conversations"),

                    #[name = "threading_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Group messages by conversation"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleThreading(row.is_active()));
                        },
                    },

                    #[name = "thread_expansion_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.threading,
                        set_title: &i18n("Expandable conversations"),
                        set_subtitle: &i18n("Off, a conversation’s messages show only in the reading pane."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleThreadExpansion(row.is_active()));
                        },
                    },

                    #[name = "threads_expanded_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.threading && model.thread_expansion,
                        set_title: &i18n("Expand conversations by default"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetThreadsExpanded(row.is_active()));
                        },
                    },

                    #[name = "thread_newest_first_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Newest message first"),
                        set_subtitle: &i18n("In the reading pane."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetThreadNewestFirst(row.is_active()));
                        },
                    },

                    #[name = "fold_messages_row"]
                    adw::ComboRow {
                        set_title: &i18n("Fold earlier messages"),
                        set_subtitle: &i18n("The newest always opens."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFoldMessages(row.selected()));
                        },
                    },

                    #[name = "thread_row_newest_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.threading,
                        set_title: &i18n("Show your own replies in the message list"),
                        set_subtitle: &i18n("Off shows the last message that arrived."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetThreadRowNewest(row.is_active()));
                        },
                    },

                    #[name = "reply_position_row"]
                    adw::ComboRow {
                        set_title: &i18n("Reply editor position"),
                        set_subtitle: &i18n("Following the reading order puts it where the conversation ends."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeReplyPosition(row.selected()));
                        },
                    },

                    #[name = "card_attachments_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Attachments on each message"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetCardAttachments(row.is_active()));
                        },
                    },

                    #[name = "attachment_drawer_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Attachment drawer"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAttachmentDrawer(row.is_active()));
                        },
                    },

                    #[name = "confirm_thread_delete_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Confirm conversation deletion"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetConfirmThreadDelete(row.is_active()));
                        },
                    },
                },
            }
        }
        Self {
            page,
            threading_row,
            thread_expansion_row,
            threads_expanded_row,
            thread_newest_first_row,
            fold_messages_row,
            thread_row_newest_row,
            reply_position_row,
            card_attachments_row,
            attachment_drawer_row,
            confirm_thread_delete_row,
        }
    }

    /// The view's `#[watch]` setters for this page, in the order the
    /// view had them (see the module comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.thread_expansion_row.set_sensitive(model.threading);
        self.threads_expanded_row
            .set_sensitive(model.threading && model.thread_expansion);
        self.thread_row_newest_row.set_sensitive(model.threading);
    }
}
