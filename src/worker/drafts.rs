//! What every backend says about saving a draft or a template.

use crate::i18n::{i18n, i18n_f};
use crate::worker::{OutgoingMessage, WorkerEvent};

/// A draft that could not be saved: said, and handed back to be opened
/// again (#340), unless the composer was saving it on its own and is
/// still open, in which case it just tries again later.
pub(super) fn draft_not_saved(
    autosave: Option<u32>,
    template: bool,
    why: String,
    message: OutgoingMessage,
    emit: &impl Fn(WorkerEvent),
) {
    if autosave.is_some() {
        tracing::info!("automatic draft save failed: {why}");
        emit(WorkerEvent::DraftSaved {
            autosave,
            message_id: None,
            template,
        });
        return;
    }
    if template {
        emit(WorkerEvent::error(i18n_f(
            "Could not save template: {e}",
            &[("e", &why)],
        )));
        // Save as Template leaves its composer open, with nothing to give
        // back. An edited template's composer has closed: the text returns
        // as a new message, cut loose from the template, so that saving it
        // cannot put a draft in the template's place.
        if message.draft_origin.is_some() {
            emit(WorkerEvent::Unsent(Box::new(OutgoingMessage {
                draft_origin: None,
                ..message
            })));
        }
        return;
    }
    emit(WorkerEvent::error(i18n_f(
        "Could not save draft: {e}",
        &[("e", &why)],
    )));
    emit(WorkerEvent::Unsent(Box::new(message)));
}

/// What the status line says while a draft, or a template, is saved.
pub(super) fn saving_text(template: bool) -> String {
    if template {
        i18n("Saving template…")
    } else {
        i18n("Saving draft…")
    }
}
