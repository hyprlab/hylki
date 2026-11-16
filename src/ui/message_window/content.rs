//! What the pop-out shows: the conversation in its reader, the attachments
//! in its drawer, and the actions taken on a member.

use super::types::MessageWindowOutput;
use super::MessageWindow;
use crate::models::{Attachment, Message};
use crate::ui::attachment_drawer::AttachmentDrawerInput;
use crate::ui::message_list::RowAction;
use crate::ui::message_view::MessageViewInput;
use relm4::prelude::*;

impl MessageWindow {
    pub(super) fn emit_action(&self, action: RowAction, sender: &ComponentSender<Self>) {
        let _ = sender.output(MessageWindowOutput::Action {
            action,
            message: Box::new(self.msg.clone()),
        });
    }

    pub(super) fn render_body(&self) {
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
    pub(super) fn member(&self, account_id: u32, id: u32) -> Option<&Message> {
        self.thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
    }

    /// The message a file of the drawer came with: the first carrying one
    /// of that name and size, since the drawer shows such a pair once.
    pub(super) fn owner(&self, att: &Attachment) -> Option<&Message> {
        let (key, _) = self.attachments.iter().find(|(_, files)| {
            files
                .iter()
                .any(|a| a.name == att.name && a.data.len() == att.data.len())
        })?;
        self.member(key.0, key.1)
    }

    /// The files on the drawer and on each card. The drawer spans the whole
    /// conversation, as the main window's does; a reply pulled in from Sent
    /// can repeat what it was sent with, so a (name, size) pair shows once.
    pub(super) fn sync_attachments(&self) {
        let (merged, rows) = merge_attachments(&self.thread, &self.attachments);
        // Switched off (#213), the drawer is simply never given anything:
        // empty hides it, seam and all.
        self.drawer
            .emit(AttachmentDrawerInput::SetItems(if self.drawer_enabled {
                merged
            } else {
                Vec::new()
            }));
        self.view.emit(MessageViewInput::SetCardAttachments(rows));
        self.fit_min_width();
    }
}

/// The window/title text for a message (its subject, or a placeholder).
pub(super) fn title_text(m: &Message) -> String {
    if m.subject.trim().is_empty() {
        "(No subject)".to_string()
    } else {
        m.subject.clone()
    }
}

/// Each card's own attachment row, by message key.
type CardRows = std::collections::HashMap<(u32, u32), Vec<crate::ui::message_view::CardAttachment>>;

/// The drawer's files and each card's row (#213), from each member's files.
/// The drawer lists a file once however many members carry it (same name
/// and size); a card lists its own, and a member without files gets no row.
fn merge_attachments(
    thread: &[Message],
    attachments: &[((u32, u32), Vec<Attachment>)],
) -> (Vec<Attachment>, CardRows) {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    let mut rows = std::collections::HashMap::new();
    for m in thread {
        let key = (m.account_id, m.id);
        let Some((_, files)) = attachments.iter().find(|(k, _)| *k == key) else {
            continue;
        };
        for a in files {
            if seen.insert((a.name.clone(), a.data.len())) {
                merged.push(a.clone());
            }
        }
        if !files.is_empty() {
            let row = files
                .iter()
                .map(|a| crate::ui::message_view::CardAttachment {
                    name: a.name.clone(),
                    size: a.data.len() as u64,
                })
                .collect::<Vec<_>>();
            rows.insert(key, row);
        }
    }
    (merged, rows)
}

#[cfg(test)]
mod tests {
    use super::{merge_attachments, title_text};
    use crate::models::{Attachment, Message};

    fn member(id: u32, subject: &str) -> Message {
        Message {
            id,
            account_id: 1,
            folder_id: 1,
            uid: id,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: subject.into(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: false,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        }
    }

    fn file(name: &str, len: usize) -> Attachment {
        Attachment {
            name: name.into(),
            data: vec![0; len].into(),
        }
    }

    #[test]
    fn the_drawer_lists_a_file_once_and_each_card_its_own() {
        let thread = [member(1, ""), member(2, ""), member(3, "")];
        let attachments = vec![
            // Not in thread order: the thread decides the order.
            ((1, 2), vec![file("a.pdf", 3), file("b.png", 5)]),
            ((1, 1), vec![file("a.pdf", 3), file("a.pdf", 4)]),
            ((1, 3), vec![]),
            ((1, 9), vec![file("stranger.txt", 1)]),
        ];
        let (merged, rows) = merge_attachments(&thread, &attachments);
        let names: Vec<(&str, usize)> = merged
            .iter()
            .map(|a| (a.name.as_str(), a.data.len()))
            .collect();
        assert_eq!(names, [("a.pdf", 3), ("a.pdf", 4), ("b.png", 5)]);
        assert_eq!(rows.len(), 2, "no row for a member without files");
        let row = |k| {
            rows[&k]
                .iter()
                .map(|c| (c.name.clone(), c.size))
                .collect::<Vec<_>>()
        };
        assert_eq!(row((1, 1)), [("a.pdf".into(), 3), ("a.pdf".into(), 4)]);
        assert_eq!(row((1, 2)), [("a.pdf".into(), 3), ("b.png".into(), 5)]);
    }

    #[test]
    fn a_window_without_a_subject_says_so() {
        assert_eq!(title_text(&member(1, "Hello")), "Hello");
        // Untranslated, a known quirk.
        assert_eq!(title_text(&member(1, "  ")), "(No subject)");
        assert_eq!(title_text(&member(1, " x ")), " x ");
    }
}
