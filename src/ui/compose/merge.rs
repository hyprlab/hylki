//! Mail Merge (#447) from the composer: the message as written, its fields
//! still in it, goes to the dialog, and what comes back is sent as one
//! message per row.

use adw::prelude::*;
use relm4::prelude::*;

use super::messages::{ComposeInput, ComposeOutput, OutgoingFields};
use super::Compose;
use super::ComposeWidgets;
use crate::config::ComposeFormat;
use crate::i18n::i18n;
use crate::mail::merge::Letter;
use crate::ui::dialog::alert;
use crate::ui::merge_dialog;

impl Compose {
    /// Handles [`ComposeInput::MailMerge`]: the message read as Send reads
    /// it, but with no check of the recipients, which are addresses only
    /// once the rows fill them in.
    pub(super) fn on_mail_merge(
        &mut self,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
        if self.encrypt {
            alert(
                parent.as_ref(),
                &i18n("Cannot merge an encrypted message"),
                &i18n("Each message would need its recipient's key. Turn off Encrypt to merge."),
            );
            return;
        }
        if !self.attachments_present(widgets, sender) {
            return;
        }
        let from_alias = match self.chosen_from(widgets) {
            Ok(from) => from,
            Err(why) => {
                alert(parent.as_ref(), &i18n("Check the From address"), &why);
                return;
            }
        };
        let fields = OutgoingFields {
            from_alias,
            ..self.draft_fields(widgets)
        };
        let s = sender.clone();
        self.read_body_as(self.format, true, move |html, text| {
            s.input_sender()
                .emit(ComposeInput::MergeBody(OutgoingFields {
                    html,
                    text,
                    ..fields.clone()
                }));
        });
    }

    /// Handles [`ComposeInput::MergeBody`]: the dialog, over the composer.
    pub(super) fn on_merge_body(
        &mut self,
        fields: OutgoingFields,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let Some(parent) = widgets.to_row.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let letter = Letter {
            to: fields.to.clone(),
            cc: fields.cc.clone(),
            bcc: fields.bcc.clone(),
            reply_to: fields.reply_to.clone(),
            subject: fields.subject.clone(),
            body: fields.html.clone(),
            text: fields.text.clone(),
        };
        let mut groups: Vec<(String, Vec<crate::contacts::Contact>)> = self
            .suggestions
            .iter()
            .filter_map(|s| Some((s.name.clone(), s.group.as_ref()?.members.clone())))
            .collect();
        groups.sort_by_key(|(name, _)| name.to_lowercase());
        let start = merge_dialog::Start {
            letter,
            format: self.format,
            groups,
        };
        let s = sender.input_sender().clone();
        merge_dialog::show(&parent, start, move |letters, interval| {
            s.emit(ComposeInput::MergeSend {
                fields: fields.clone(),
                letters,
                interval,
            });
        });
    }

    /// Handles [`ComposeInput::MergeSend`]: each row's message, made as Send
    /// makes one, and the composer closes.
    pub(super) fn on_merge_send(
        &mut self,
        fields: OutgoingFields,
        letters: Vec<Letter>,
        interval: Option<std::time::Duration>,
        sender: &ComponentSender<Self>,
    ) {
        let mut messages = Vec::with_capacity(letters.len());
        for (i, l) in letters.into_iter().enumerate() {
            let mut out = self.outgoing_message(OutgoingFields {
                html: l.body,
                text: l.text,
                to: l.to,
                cc: l.cc,
                bcc: l.bcc,
                reply_to: l.reply_to,
                subject: l.subject,
                ..fields.clone()
            });
            if self.format != ComposeFormat::Plain {
                out.html = crate::mail::direction::stamp(&out.html);
            }
            out.send_at = None;
            out.merged = true;
            // The draft or queued message this was written from is
            // replaced once, by the first; a template stays to be used
            // again.
            if i > 0 || self.template {
                out.draft_origin = None;
            }
            if i > 0 {
                out.outbox_origin = None;
            }
            messages.push(out);
        }
        if messages.is_empty() {
            return;
        }
        let _ = sender.output(ComposeOutput::SendMerged { messages, interval });
        let _ = sender.output(ComposeOutput::Close(self.compose_id));
    }
}
