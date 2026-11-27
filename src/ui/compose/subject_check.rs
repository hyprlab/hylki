//! The empty subject check at Send (#726): a message with no subject is
//! asked about first, unless Settings → Composing turns the question off.
//! It comes before the attachment reminder, which still runs after Send
//! Anyway.

use std::cell::RefCell;

use super::messages::{ComposeInput, OutgoingFields};
use super::Compose;
use super::ComposeWidgets;
use crate::i18n::i18n;
use adw::prelude::*;
use relm4::prelude::*;

/// Whether a subject counts as missing: nothing but spaces counts too.
pub(super) fn is_missing(subject: &str) -> bool {
    subject.trim().is_empty()
}

impl Compose {
    /// Whether Send stops to ask about the subject. Read at each Send, so
    /// an open composer follows the switch.
    pub(super) fn asks_for_subject(&self, fields: &OutgoingFields) -> bool {
        is_missing(&fields.subject) && crate::config::load_ask_empty_subject()
    }

    /// Ask whether to send without a subject. Send Anyway goes on to the
    /// attachment reminder with `fields`; Cancel, the default, puts the
    /// cursor in the subject.
    #[allow(deprecated)] // adw::MessageDialog, as in crate::ui::dialog
    pub(super) fn ask_send_untitled(
        &self,
        fields: OutgoingFields,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
        let dialog = crate::ui::dialog::message_dialog(
            parent.as_ref(),
            Some(&i18n("Send Without a Subject?")),
            Some(&i18n("The message has no subject.")),
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
                s.emit(ComposeInput::FocusSubject);
                return;
            }
            if let Some(fields) = pending.borrow_mut().take() {
                s.emit(ComposeInput::SendUntitled(fields));
            }
        });
        dialog.present();
    }

    /// Handles [`ComposeInput::FocusSubject`]: the field rows of a compact
    /// reply are shown first, so the subject is there to type in.
    pub(super) fn on_focus_subject(&self, widgets: &ComposeWidgets) {
        if !widgets.fields_list.is_visible() {
            // The chevron's toggle sends ShowFields, which records it.
            widgets.fields_btn.set_active(true);
            widgets.fields_list.set_visible(true);
        }
        widgets.subject_row.set_visible(true);
        widgets.subject_row.grab_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::is_missing;

    #[test]
    fn blank_subjects_are_missing() {
        assert!(is_missing(""));
        assert!(is_missing("   "));
        assert!(is_missing("\t \u{a0}"));
        assert!(!is_missing("Re: lunch"));
        assert!(!is_missing(" x "));
    }
}
