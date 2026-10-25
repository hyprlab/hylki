//! A reply sent from the split pane turning into its card in the
//! conversation, and the card standing in until the Sent copy arrives.

use adw::prelude::*;
use relm4::prelude::*;

use crate::models::{Attachment, FolderKind, Message};
use crate::ui::message_view::{CardAttachment, MessageViewInput};
use crate::worker::{self, OutgoingMessage};

use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::first_recipient;

/// A reply on its way from the split pane into its conversation: its card,
/// the composer it was written in, and the conversation it answers.
pub(super) struct MorphPending {
    message: Message,
    composer: gtk::Widget,
    conversation: (u32, u32),
}

/// The card a reply sent from the split pane has in its conversation until
/// its copy is read back from Sent (send_morph.rs).
pub(super) struct Provisional {
    pub(super) message: Message,
    /// The Message-ID it answers.
    pub(super) answers: String,
}

/// A file a reply sent from here went with, as its card lists it, and
/// where it is on disk for opening it from the card.
pub(super) struct SentFile {
    card: CardAttachment,
    path: String,
}

impl SentFile {
    /// Named as the sent message names it (see `attachments_multipart`).
    fn new(path: &str) -> Self {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".to_string());
        let size = std::fs::metadata(path).map_or(0, |m| m.len());
        SentFile {
            card: CardAttachment { name, size },
            path: path.to_string(),
        }
    }
}

impl Provisional {
    /// Whether `m` is this reply's own copy: from the same account, dated
    /// when this was sent (by this machine's clock either way), and
    /// answering the same message.
    pub(super) fn copied_by(&self, m: &Message) -> bool {
        m.uid != 0
            && m.account_id == self.message.account_id
            && m.timestamp >= self.message.timestamp - 30
            && m.references.split_whitespace().any(|x| x == self.answers)
    }
}

impl AppModel {
    /// Whether a message the composer `from` just sent becomes its own card
    /// in the conversation: a reply to the conversation on screen, sent now
    /// from the split pane beside it.
    pub(super) fn reply_joins(&self, from: u32, out: &OutgoingMessage) -> bool {
        let skip = |why: &str| {
            tracing::info!(target: "hylki::morph", "no send morph: {why}");
            false
        };
        let Some(r) = self.reader_compose.as_ref().filter(|r| r.id == from) else {
            return false;
        };
        if r.window.is_some() || self.showing_contacts || self.live_split().is_none() {
            return skip("not sent from the split beside the reader");
        }
        // A message sent later is not in Sent yet; one that starts a
        // conversation does not join this one.
        if out.send_at.is_some() {
            return skip("sent later");
        }
        let Some(current) = self.current.as_ref() else {
            return skip("nothing open in the reader");
        };
        // The reply answers a card of the conversation on screen: the one
        // the split was opened from, or failing that, by its Message-ID.
        let joins = std::iter::once(current)
            .chain(self.current_thread.iter())
            .any(|m| {
                r.answering == Some((m.account_id, m.id))
                    || (!out.in_reply_to.is_empty() && m.message_id == out.in_reply_to)
            });
        if !joins {
            return skip("answers a message not in the conversation on screen");
        }
        true
    }

    /// The card a sent reply has in the conversation until its copy is read
    /// back from Sent: the same body that copy will have, filed where it
    /// will be filed.
    pub(super) fn provisional_message(&mut self, out: &OutgoingMessage) -> Option<Message> {
        let account_id = out.from_account_id;
        let account = self
            .effective_config()
            .get(account_id.checked_sub(1)? as usize)?;
        // The body its copy will have; failing that, what was written.
        let body = worker::sent_body_preview(account, out).unwrap_or_else(|| {
            tracing::info!(target: "hylki::morph", "the reply's card shows the text as written");
            worker::wrap_plain(&out.body)
        });
        let (from_name, from_addr) = out
            .from_alias
            .as_deref()
            .and_then(first_recipient)
            .unwrap_or_else(|| (account.name.clone(), account.email.clone()));
        let folder_id =
            worker::reply_copy_path(account, account_id, self.cache.as_ref(), &out.in_reply_to)
                .or_else(|| self.sent_copy_path(account_id))
                .and_then(|p| self.folder_by_path(account_id, &p).map(|f| f.id))
                .or_else(|| {
                    self.folder_of_kind(account_id, FolderKind::Sent)
                        .map(|f| f.id)
                })
                .or_else(|| self.current.as_ref().map(|m| m.folder_id))?;
        self.related_id_seq = self.related_id_seq.saturating_sub(1);
        // The card lists its files from the moment it is on screen, not
        // once the copy has been sent, appended and read back.
        if !out.attachments.is_empty() {
            self.sent_files.insert(
                (account_id, self.related_id_seq),
                out.attachments.iter().map(|p| SentFile::new(p)).collect(),
            );
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Some(Message {
            id: self.related_id_seq,
            account_id,
            folder_id,
            uid: 0,
            from_name,
            from_addr,
            reply_to: out.reply_to.clone(),
            to: out.to.clone(),
            cc: out.cc.clone(),
            bcc: out.bcc.clone(),
            subject: out.subject.clone(),
            preview: out.body.chars().take(200).collect(),
            body,
            date: String::new(),
            timestamp,
            unread: false,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: !out.attachments.is_empty(),
            message_id: String::new(),
            references: format!("{} {}", out.references, out.in_reply_to)
                .trim()
                .to_string(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        })
    }

    /// Start turning the sent split reply into its card (send_morph.rs).
    /// The composer stays on screen until a still of the pane has been
    /// taken and the conversation's cards measured; then the pane changes
    /// under the stage ([`Self::send_morph_swap`]). False when there is no
    /// split to morph from; the composer then slides away as it always has.
    pub(super) fn begin_send_morph(&mut self, message: Message, composer: &gtk::Widget) -> bool {
        use crate::ui::send_morph::{find_widget, Pieces};
        let Some((slot, _, _)) = self.live_split() else {
            return false;
        };
        let reader = self.reader_split_lower.start_child();
        let view = self.reader_web_view();
        let editor = find_widget(composer, &|w| w.has_css_class("compose-editor-card"));
        let (Some(reader), Some(view), Some(editor)) = (reader, view, editor) else {
            tracing::info!(target: "hylki::morph", "no send morph: the pane's parts were not found");
            return false;
        };
        if self.send_morph.parent().is_none() {
            return false;
        }
        let Some(conversation) = self.current.as_ref().map(|c| (c.account_id, c.id)) else {
            return false;
        };
        tracing::info!(target: "hylki::morph", "send morph: starting");
        self.morph_pending = Some(MorphPending {
            message,
            composer: composer.clone(),
            conversation,
        });
        let header = self
            .reader_header
            .get()
            .map(|h| h.clone().upcast::<gtk::Widget>());
        let header_shown = header
            .as_ref()
            .and_then(|h| h.ancestor(adw::ToolbarView::static_type()))
            .and_downcast::<adw::ToolbarView>()
            .is_some_and(|tv| tv.reveals_top_bars());
        let editor_view = find_widget(&editor, &|w| w.is::<webkit6::WebView>())
            .and_downcast::<webkit6::WebView>();
        let pieces = Pieces {
            root: self.reader_split.clone().upcast(),
            composer: slot.upcast(),
            editor,
            reader,
            view,
            header,
            header_shown,
            editor_view,
        };
        let s = self.self_sender.clone();
        crate::ui::send_morph::start(&self.send_morph, pieces, move |ok| {
            s.emit(if ok {
                AppMsg::SendMorphMeasured
            } else {
                AppMsg::SendMorphFailed
            });
        });
        true
    }

    /// The pane could not be captured for the send morph: the composer
    /// slides away instead, and the reply joins its conversation plainly.
    fn send_morph_failed(&mut self) {
        let Some(pending) = self.morph_pending.take() else {
            return;
        };
        // The reply never leaves the pane: the composer goes at once and
        // the card is in the conversation in the same moment.
        if self.composer_in_split(&pending.composer) {
            self.reveal_reader_header_at_once();
            self.clear_compose_slots();
        }
        self.join_conversation(pending.message, pending.conversation, true);
    }

    /// The pane changes under the send morph's stage: the split goes at
    /// once, the reader has its header back, and the reply joins the
    /// conversation pinned in view. Once its card has settled the stage
    /// carries the editor to it. A morph ended early (another message
    /// opened, a new reply begun) leaves only the reply joining.
    fn send_morph_swap(&mut self) {
        let Some(pending) = self.morph_pending.take() else {
            return;
        };
        let morphing = self.send_morph.running();
        if morphing {
            self.send_morph.show();
            self.reveal_reader_header_at_once();
        }
        // Only the composer that sent it: a new reply may hold the slot now.
        if self.composer_in_split(&pending.composer) {
            if !morphing {
                self.reveal_reader_header_at_once();
            }
            self.clear_compose_slots();
        }
        let (account_id, id) = (pending.message.account_id, pending.message.id);
        let joined = self.join_conversation(pending.message, pending.conversation, true);
        if !morphing {
            return;
        }
        let root: gtk::Widget = self.reader_split.clone().upcast();
        match (
            self.reader_split_lower.start_child(),
            self.reader_web_view(),
        ) {
            (Some(reader), Some(view)) if joined => {
                let header = self
                    .reader_header
                    .get()
                    .map(|h| h.clone().upcast::<gtk::Widget>());
                crate::ui::send_morph::await_arrival(
                    &self.send_morph,
                    &root,
                    &reader,
                    &view,
                    header,
                    format!("{account_id}:{id}"),
                );
            }
            _ => self.send_morph.abort(),
        }
    }

    /// The reader's web view, which shows the conversation.
    fn reader_web_view(&self) -> Option<webkit6::WebView> {
        crate::ui::send_morph::find_widget(self.message_view.widget().upcast_ref(), &|w| {
            w.is::<webkit6::WebView>()
        })
        .and_downcast::<webkit6::WebView>()
    }

    /// Whether `composer` is still the one in the split slot.
    fn composer_in_split(&self, composer: &gtk::Widget) -> bool {
        self.live_split()
            .is_some_and(|(slot, _, _)| composer.is_ancestor(&slot))
    }

    /// Put a sent reply's card in the conversation it answers, if that is
    /// the one on screen, pinned in view when `pin`; either way it stands in
    /// until the copy is read back from Sent. True when it went on screen.
    pub(super) fn join_conversation(
        &mut self,
        message: Message,
        conversation: (u32, u32),
        pin: bool,
    ) -> bool {
        let (account_id, id) = (message.account_id, message.id);
        let answers = message
            .references
            .split_whitespace()
            .last()
            .unwrap_or_default()
            .to_string();
        self.provisionals.insert(
            (account_id, id),
            Provisional {
                message: message.clone(),
                answers,
            },
        );
        let Some(current) = self
            .current
            .clone()
            .filter(|c| (c.account_id, c.id) == conversation)
        else {
            return false;
        };
        if self.current_thread.is_empty() {
            self.current_thread.push(current);
        }
        self.current_thread.push(message);
        // Ahead of the render, so the document is built with them.
        self.push_card_attachments();
        // A document is on screen already: the next one replaces it in
        // place, never behind the spinner.
        self.thread_painted = true;
        if pin {
            self.message_view
                .emit(MessageViewInput::PinArrival { account_id, id });
        }
        self.show_thread();
        true
    }

    /// Take a sent reply's stand-in card out of the conversation: its send
    /// was refused, and the composer is back.
    pub(super) fn drop_provisional(&mut self, in_reply_to: &str) {
        let refused: Vec<(u32, u32)> = self
            .provisionals
            .iter()
            .filter(|(_, p)| p.answers == in_reply_to)
            .map(|(k, _)| *k)
            .collect();
        if refused.is_empty() {
            return;
        }
        for k in &refused {
            self.provisionals.remove(k);
            self.sent_files.remove(k);
        }
        let before = self.current_thread.len();
        self.current_thread
            .retain(|m| !refused.contains(&(m.account_id, m.id)));
        if self.current_thread.len() != before {
            self.show_thread();
        }
    }

    /// What the card `key` lists as a reply sent from here, until its copy's
    /// own attachments are read back.
    pub(super) fn sent_card_files(&self, key: (u32, u32)) -> Option<Vec<CardAttachment>> {
        let files = self.sent_files.get(&key)?;
        Some(files.iter().map(|f| f.card.clone()).collect())
    }

    /// The files of the card `key` as sent, read from disk for a click on
    /// one before its copy's own are in hand. `None` when any is gone.
    pub(super) fn sent_file_bytes(&self, key: (u32, u32)) -> Option<Vec<Attachment>> {
        self.sent_files
            .get(&key)?
            .iter()
            .map(|f| {
                let data = std::fs::read(&f.path).ok()?;
                Some(Attachment {
                    name: f.card.name.clone(),
                    data,
                })
            })
            .collect()
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_send_morph(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::SendMorphMeasured => self.send_morph_swap(),
            AppMsg::SendMorphFailed => self.send_morph_failed(),
            _ => unreachable!("not routed to on_send_morph"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    /// A file is named on the card as the sent message names it, and sized
    /// from disk before anything has gone out.
    #[test]
    fn a_sent_file_is_listed_by_its_name_and_size() {
        let dir = std::env::temp_dir().join(format!("hylki-sent-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.pdf");
        std::fs::write(&path, b"12345").unwrap();
        let f = SentFile::new(path.to_str().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(f.card.name, "report.pdf");
        assert_eq!(f.card.size, 5);
    }

    /// B2 in the map: an empty `answers` never matches, since an empty
    /// reference is never in a whitespace split.
    #[test]
    fn a_provisional_card_is_copied_by_its_sent_copy() {
        let mut sent = msg(1, 5, 0);
        sent.timestamp = 1000;
        let p = Provisional {
            message: sent.clone(),
            answers: "parent@x".into(),
        };
        let mut copy = msg(1, 9, 77);
        copy.timestamp = 970;
        copy.references = "root@x parent@x".into();
        assert!(p.copied_by(&copy));
        copy.timestamp = 969;
        assert!(!p.copied_by(&copy));
        copy.timestamp = 2000;
        copy.uid = 0;
        assert!(!p.copied_by(&copy));
        copy.uid = 77;
        copy.account_id = 2;
        assert!(!p.copied_by(&copy));
        let empty = Provisional {
            message: sent,
            answers: String::new(),
        };
        copy.account_id = 1;
        assert!(!empty.copied_by(&copy));
    }
}
