//! Settings → Composing: format, reply and signature placement, undo
//! send, spelling and grammar.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct ComposingPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) compose_inline_row: adw::SwitchRow,
    pub(in crate::ui::preferences) reply_inline_row: adw::SwitchRow,
    pub(in crate::ui::preferences) reply_fields_row: adw::SwitchRow,
    pub(in crate::ui::preferences) default_from_row: adw::ComboRow,
    pub(in crate::ui::preferences) paste_plain_row: adw::SwitchRow,
    pub(in crate::ui::preferences) return_paragraph_row: adw::SwitchRow,
    pub(in crate::ui::preferences) toolbar_row: adw::ComboRow,
    pub(in crate::ui::preferences) compose_format_row: adw::ComboRow,
    pub(in crate::ui::preferences) compose_font_row: adw::ComboRow,
    pub(in crate::ui::preferences) compose_size_row: adw::ComboRow,
    pub(in crate::ui::preferences) compose_zoom_row: adw::ComboRow,
    pub(in crate::ui::preferences) follow_plain_row: adw::SwitchRow,
    pub(in crate::ui::preferences) signature_position_row: adw::ComboRow,
    pub(in crate::ui::preferences) signature_dashes_row: adw::SwitchRow,
    pub(in crate::ui::preferences) fold_quote_row: adw::SwitchRow,
    pub(in crate::ui::preferences) undo_send_row: adw::ComboRow,
    pub(in crate::ui::preferences) request_receipts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) returned_receipts_row: adw::ComboRow,
    pub(in crate::ui::preferences) reply_header_row: adw::EntryRow,
    /// The reply header as last saved, as the row shows it: the apply
    /// button is up only while the text differs from it.
    reply_header_saved: Rc<RefCell<String>>,
    pub(in crate::ui::preferences) spelling_group: adw::PreferencesGroup,
    pub(in crate::ui::preferences) spellcheck_row: adw::SwitchRow,
    pub(in crate::ui::preferences) spell_lang_row: adw::ExpanderRow,
    pub(in crate::ui::preferences) spell_words_row: adw::ExpanderRow,
}

impl ComposingPage {
    pub(in crate::ui::preferences) fn build(sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Composing"),

                    #[name = "compose_inline_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Compose in the main window"),
                        set_subtitle: &i18n("Off opens new messages in their own window."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetComposeInline(row.is_active()));
                        },
                    },

                    #[name = "reply_inline_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Reply and forward in the main window"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetReplyInline(row.is_active()));
                        },
                    },

                    #[name = "reply_fields_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show From, To and Subject in the reply panel"),
                        set_subtitle: &i18n("Off folds them behind a button."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetReplyFields(row.is_active()));
                        },
                    },

                    #[name = "default_from_row"]
                    adw::ComboRow {
                        set_title: &i18n("Send new messages from"),
                        set_subtitle: &i18n("Replies answer from the address they were sent to."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeComposeDefaultFrom(row.selected()));
                        },
                    },

                    #[name = "paste_plain_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Paste as plain text"),
                        set_subtitle: &i18n("Right-clicking the editor offers both either way."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetPastePlain(row.is_active()));
                        },
                    },

                    #[name = "return_paragraph_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Return starts a new paragraph"),
                        set_subtitle: &i18n("Shift+Return does the other."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetReturnParagraph(row.is_active()));
                        },
                    },

                    #[name = "toolbar_row"]
                    adw::ComboRow {
                        set_title: &i18n("Formatting toolbar"),
                        set_subtitle: &i18n("For new messages. The toolbar's chevron toggles it."),
                        set_model: Some(&gtk::StringList::new(&[
                            &i18n("Always collapsed"),
                            &i18n("Always expanded"),
                        ])),
                        connect_selected_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetToolbarExpanded(row.selected() == 1));
                        },
                    },

                    #[name = "compose_format_row"]
                    adw::ComboRow {
                        set_title: &i18n("Start new messages in"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeComposeFormat(row.selected()));
                        },
                    },

                    // Filled and connected in init: the
                    // list grows with the fonts picked
                    // through Other Font….
                    #[name = "compose_font_row"]
                    adw::ComboRow {
                        set_title: &i18n("Font"),
                        set_subtitle: &i18n("For rich text. Default leaves it to the recipient's own font."),
                    },

                    #[name = "compose_size_row"]
                    adw::ComboRow {
                        set_title: &i18n("Font size"),
                        set_subtitle: &i18n("For rich text, in points."),
                    },

                    #[name = "compose_zoom_row"]
                    adw::ComboRow {
                        set_title: &i18n("Default zoom"),
                    },

                    #[name = "follow_plain_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Reply in the original's format"),
                        set_subtitle: &i18n("Plain-text mail is answered in plain text."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetFollowPlain(row.is_active()));
                        },
                    },

                    #[name = "signature_position_row"]
                    adw::ComboRow {
                        set_title: &i18n("Signature in replies"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeSignaturePosition(row.selected()));
                        },
                    },

                    #[name = "signature_dashes_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Separator line above the signature"),
                        set_subtitle: &i18n("The standard “-- ” line, which other mail apps recognize."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSignatureDashes(row.is_active()));
                        },
                    },

                    #[name = "fold_quote_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Fold quoted text"),
                        set_subtitle: &i18n("Replies and forwards open with the original behind •••."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetFoldQuote(row.is_active()));
                        },
                    },

                    #[name = "undo_send_row"]
                    adw::ComboRow {
                        set_title: &i18n("Undo Send"),
                        set_subtitle: &i18n("How long Undo is offered after sending."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeUndoSend(row.selected()));
                        },
                    },

                    #[name = "request_receipts_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Request read receipts"),
                        set_subtitle: &i18n("Not every mail app sends one."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetRequestReceipts(row.is_active()));
                        },
                    },

                    #[name = "returned_receipts_row"]
                    adw::ComboRow {
                        set_title: &i18n("Returned receipts"),
                        set_subtitle: &i18n("Sent mail shows who read it; the receipt goes here."),
                        set_model: Some(&gtk::StringList::new(&[
                            &i18n("Move to Trash"),
                            &i18n("Archive"),
                            &i18n("Keep in Inbox"),
                        ])),
                        connect_selected_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetReturnedReceipts(
                                crate::ui::preferences::choices::returned_receipts_at(row.selected()),
                            ));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Reply Header"),
                    set_description: Some(&i18n("{name}, {address}, {date} and {time} are filled in from the original.")),

                    // Saved on Apply; the text is set and wired in build
                    // below, beside the button that restores the default.
                    #[name = "reply_header_row"]
                    adw::EntryRow {
                        set_title: &i18n("Line above the quoted message"),
                        set_show_apply_button: true,
                        add_suffix: reply_header_reset = &gtk::Button {
                            set_icon_name: "edit-undo-symbolic",
                            set_valign: gtk::Align::Center,
                            set_tooltip_text: Some(i18n("Restore Default").as_str()),
                            add_css_class: "flat",
                        },
                    },
                },

                #[name = "spelling_group"]
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Spelling"),
                    // The description is filled in at init with the
                    // dictionaries the app can actually see — checking a
                    // language without one silently checks nothing, so
                    // honesty about what is installed beats silence.

                    #[name = "spellcheck_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Check spelling as you type"),
                        set_subtitle: &i18n("Right-click a word for corrections."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSpellcheck(row.is_active()));
                        },
                    },

                    // One switch per installed dictionary, filled
                    // in at init (#365); the subtitle names the
                    // languages in use.
                    #[name = "spell_lang_row"]
                    adw::ExpanderRow {
                        set_title: &i18n("Languages"),
                    },

                    // Words the user taught the checker ("Learn Spelling"
                    // in the composer, or added right here). Populated
                    // and maintained imperatively in init.
                    #[name = "spell_words_row"]
                    adw::ExpanderRow {
                        set_title: &i18n("Added words"),
                        set_subtitle: &i18n("Changes apply after a restart."),
                    },
                },
            }
        }
        page.add(&super::super::grammar::group());
        let reply_header_saved = Rc::new(RefCell::new(String::new()));
        wire_reply_header(
            &reply_header_row,
            &reply_header_reset,
            &reply_header_saved,
            sender,
        );
        Self {
            page,
            compose_inline_row,
            reply_inline_row,
            reply_fields_row,
            default_from_row,
            paste_plain_row,
            return_paragraph_row,
            toolbar_row,
            compose_format_row,
            compose_font_row,
            compose_size_row,
            compose_zoom_row,
            follow_plain_row,
            signature_position_row,
            signature_dashes_row,
            fold_quote_row,
            undo_send_row,
            request_receipts_row,
            returned_receipts_row,
            reply_header_row,
            reply_header_saved,
            spelling_group,
            spellcheck_row,
            spell_lang_row,
            spell_words_row,
        }
    }
}

impl ComposingPage {
    /// Show the saved reply header, "" for the default.
    pub(in crate::ui::preferences) fn set_reply_header(&self, stored: &str) {
        let text = shown_template(stored);
        set_quietly(&self.reply_header_row, &text);
        *self.reply_header_saved.borrow_mut() = text;
    }
}

/// The text the row shows for a stored header, "" being the default.
fn shown_template(stored: &str) -> String {
    if stored.is_empty() {
        crate::mail::attribution::default_template()
    } else {
        stored.to_string()
    }
}

/// Set the row's text without raising its apply button, which libadwaita
/// raises for any change made while the row has focus, the app's own
/// included: the text put back after Apply looked like an edit not yet
/// saved.
fn set_quietly(row: &adw::EntryRow, text: &str) {
    row.set_show_apply_button(false);
    if row.text() != text {
        row.set_text(text);
    }
    row.set_show_apply_button(true);
}

/// The reply header row saves what Apply confirms, and its button goes back
/// to the default, shown only while the text is something else. The apply
/// button goes down again when the text is edited back to what is saved.
fn wire_reply_header(
    row: &adw::EntryRow,
    reset: &gtk::Button,
    saved: &Rc<RefCell<String>>,
    sender: &ViewSender,
) {
    use crate::mail::attribution::stored_template;
    {
        let (reset, saved) = (reset.clone(), saved.clone());
        row.connect_changed(move |row| {
            reset.set_visible(!stored_template(&row.text()).is_empty());
            if row.text().trim() == saved.borrow().trim() {
                // After the row's own handler, which raises the button.
                let (row, saved) = (row.clone(), saved.clone());
                gtk::glib::idle_add_local_once(move || {
                    if row.text().trim() == saved.borrow().trim() {
                        row.set_show_apply_button(false);
                        row.set_show_apply_button(true);
                    }
                });
            }
        });
    }
    {
        let (saved, sender) = (saved.clone(), sender.clone());
        row.connect_apply(move |row| {
            let stored = stored_template(&row.text());
            let text = shown_template(&stored);
            set_quietly(row, &text);
            *saved.borrow_mut() = text;
            let _ = sender.output(PrefOutput::SetReplyHeader(stored));
        });
    }
    {
        let (row, saved, sender) = (row.clone(), saved.clone(), sender.clone());
        reset.connect_clicked(move |_| {
            let text = shown_template("");
            set_quietly(&row, &text);
            *saved.borrow_mut() = text;
            let _ = sender.output(PrefOutput::SetReplyHeader(String::new()));
        });
    }
}
