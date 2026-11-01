//! The subject row: edits that dirty the draft, spelling underlines painted
//! onto its inner text (#114), and the mail merge fields button (#447).

use super::merge_fields::FieldTarget;
use super::messages::ComposeInput;
use super::{Compose, ComposeWidgets};
use adw::prelude::*;
use relm4::prelude::*;

/// The subject marks the draft edited, and gets spelling underlines.
pub(super) fn wire_subject(widgets: &ComposeWidgets, sender: &ComponentSender<Compose>) {
    // Not focusable, so the cursor the field goes in at stays where it is.
    let fields = gtk::Button::from_icon_name("hylki-merge-field-symbolic");
    fields.set_tooltip_text(Some(crate::i18n::i18n("Insert Merge Field").as_str()));
    fields.add_css_class("flat");
    fields.set_valign(gtk::Align::Center);
    fields.set_can_focus(false);
    fields.set_focus_on_click(false);
    {
        let s = sender.input_sender().clone();
        fields.connect_clicked(move |_| {
            let _ = s.send(ComposeInput::MergeFields(FieldTarget::Subject));
        });
    }
    widgets.subject_row.add_suffix(&fields);

    // Subject edits also count as dirtying the draft.
    let s = sender.clone();
    widgets
        .subject_row
        .connect_changed(move |_| s.input_sender().emit(ComposeInput::MarkFieldsDirty));
    // The subject gets the body's red underlines too (#114): every edit
    // re-checks the line through enchant directly and paints error
    // underlines onto the row's inner GtkText — the row itself exposes
    // no Pango attributes. The word the cursor sits in is exempt while
    // typing and joins the check after a 400ms pause, so mistakes show
    // before the space without half-words flashing red mid-keystroke.
    // Addresses stay uncheckable on purpose: only the subject is prose.
    if let Some(text) = inner_text(widgets.subject_row.upcast_ref()) {
        let t = text.clone();
        let pending: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        widgets.subject_row.connect_changed(move |row| {
            if let Some(prev) = pending.borrow_mut().take() {
                prev.remove();
            }
            let content = row.text().to_string();
            // Editable positions count characters; attribute ranges
            // count bytes.
            let cursor = content
                .char_indices()
                .nth(row.position().max(0) as usize)
                .map(|(b, _)| b)
                .unwrap_or(content.len());
            t.set_attributes(crate::desktop::spell::error_attrs(&content, Some(cursor)).as_ref());
            let t = t.clone();
            let row = row.clone();
            let slot = pending.clone();
            let id = gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(400),
                move || {
                    slot.borrow_mut().take();
                    t.set_attributes(
                        crate::desktop::spell::error_attrs(&row.text(), None).as_ref(),
                    );
                },
            );
            *pending.borrow_mut() = Some(id);
        });
        // Prefilled subjects (replies, drafts) get checked on open too.
        text.set_attributes(
            crate::desktop::spell::error_attrs(&widgets.subject_row.text(), None).as_ref(),
        );
    }
}

/// The GtkText embedded somewhere inside a composite row — where Pango
/// attributes (the spell-check underlines) actually live.
fn inner_text(widget: &gtk::Widget) -> Option<gtk::Text> {
    if let Some(t) = widget.downcast_ref::<gtk::Text>() {
        return Some(t.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(t) = inner_text(&c) {
            return Some(t);
        }
        child = c.next_sibling();
    }
    None
}
