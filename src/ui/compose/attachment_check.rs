//! The attachment reminder at Send: a message whose text mentions an
//! attachment, with nothing attached, is asked about first. The words are
//! set in Settings → Composing; see [`crate::mail::attachment_reminder`].

use std::cell::RefCell;

use super::messages::{ComposeInput, OutgoingFields};
use super::Compose;
use super::ComposeWidgets;
use crate::config::ComposeFormat;
use crate::i18n::{i18n, i18n_f};
use crate::mail::attachment_reminder::{keywords, mentioned, without_signature};
use crate::ui::rich_editor::SourceKind;
use adw::prelude::*;
use relm4::prelude::*;

impl Compose {
    /// The word of the message's own text (quotes left out) that mentions
    /// an attachment, when nothing is attached; `None` sends at once.
    pub(super) fn unattached_mention(&self, fields: &OutgoingFields) -> Option<String> {
        if self.has_attachment(&fields.html) {
            return None;
        }
        let words = keywords(&crate::config::load_attachment_keywords());
        if words.is_empty() {
            return None;
        }
        mentioned(&self.written_text(fields), &words)
    }

    /// Whether anything goes with the message: a file, a cloud link (or one
    /// still uploading), or a picture put in the text.
    fn has_attachment(&self, html: &str) -> bool {
        !self.attachments.is_empty()
            || !self.cloud_links.is_empty()
            || self.cloud_busy > 0
            || (self.format == ComposeFormat::Rich && html.contains("src=\"data:image/"))
    }

    /// The text of the message with quoted lines marked `>` and the
    /// signature left out: Markdown is its own source, and the editor's HTML
    /// (rich, plain or hand-written) is turned into text, where a quoted
    /// original becomes `>` lines.
    fn written_text(&self, fields: &OutgoingFields) -> String {
        match self.format {
            ComposeFormat::Markdown => self.without_markdown_signature(&fields.html),
            _ => crate::mail::markdown::html_to_text(&without_signature(&fields.html)),
        }
    }

    /// Markdown source without its signature, found by its text as the
    /// signature swap finds it, with or without the `-- ` line.
    fn without_markdown_signature(&self, source: &str) -> String {
        let mut text = source.to_string();
        for dashes in [self.sig_dashes, !self.sig_dashes] {
            let sig = super::signature::sig_source(SourceKind::Markdown, &self.current_sig, dashes);
            let sig = sig.trim();
            if let Some(at) = text.rfind(sig).filter(|_| !sig.is_empty()) {
                text.replace_range(at..at + sig.len(), "");
                break;
            }
        }
        text
    }

    /// Ask whether to send without an attachment, naming the `word` that
    /// mentions one. Send Anyway sends `fields` as they are; Cancel, the
    /// default, goes back to the message.
    #[allow(deprecated)] // adw::MessageDialog, as in crate::ui::dialog
    pub(super) fn ask_send_unattached(
        &self,
        word: &str,
        fields: OutgoingFields,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
        let dialog = adw::MessageDialog::new(
            parent.as_ref(),
            Some(&i18n("Send without an attachment?")),
            Some(&i18n_f(
                "The message mentions “{word}”, but nothing is attached.",
                &[("word", word)],
            )),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("send", &i18n("Send Anyway"));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        let pending = RefCell::new(Some(fields));
        let s = sender.input_sender().clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "send" {
                return;
            }
            if let Some(fields) = pending.borrow_mut().take() {
                s.emit(ComposeInput::SendAnyway(fields));
            }
        });
        dialog.present();
    }
}
