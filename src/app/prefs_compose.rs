//! Applying composing settings.

use relm4::prelude::*;

use crate::ui::preferences::{PrefInput, PrefOutput};

use super::model::AppModel;
use super::msg::AppMsg;

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_compose(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::ComposeHistory { id, undo, redo } => {
                // Only the inline composer's: a composer in a window of its
                // own answers its keys there and has no menu here to label.
                if self
                    .reader_compose
                    .as_ref()
                    .is_some_and(|r| r.id == id && r.window.is_none())
                {
                    self.compose_history = (undo, redo);
                    self.refresh_undo_menu();
                }
            }
            _ => unreachable!("not routed to on_prefs_compose"),
        }
    }

    /// The settings changes the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_compose_pref(&mut self, pref: PrefOutput) {
        match pref {
            PrefOutput::SetComposeInline(on) => {
                pref!(self.compose_inline = on);
            }

            PrefOutput::SetReplyInline(on) => {
                pref!(self.reply_inline = on);
            }

            PrefOutput::SetReplyFields(on) => {
                pref!(self.reply_fields = on);
            }

            PrefOutput::SetFilesPrefs(prefs) => {
                pref!(self.files_prefs = prefs);
            }

            PrefOutput::SetLinkBrowser(choice) => {
                if self.link_browser != choice {
                    self.link_browser = choice;
                    crate::ui::launch::set_browser(&self.link_browser);
                    self.save_settings();
                }
            }

            PrefOutput::SetComposeDefaultFrom(addr) => {
                pref!(self.compose_default_from = addr);
            }

            PrefOutput::SetPastePlain(on) => {
                pref!(self.paste_plain = on);
            }

            PrefOutput::SetReturnParagraph(on) => {
                pref!(self.return_paragraph = on);
            }

            PrefOutput::SetToolbarExpanded(on) => {
                pref!(self.toolbar_expanded = on);
            }

            PrefOutput::SetComposeFont(font) => {
                pref!(self.compose_font = font);
            }

            PrefOutput::SetComposeFontSize(pt) => {
                pref!(self.compose_font_size = pt);
            }

            PrefOutput::SetComposeZoom(zoom) => {
                if pref!(self.compose_zoom = zoom) {
                    // The next composer opens at the new default, not at
                    // the size the last one was left at.
                    crate::ui::compose::forget_zoom();
                }
            }

            PrefOutput::SetPrintOptions(options) => {
                if pref!(self.print_options = options) {
                    crate::ui::print_preview::set_options(options);
                }
            }

            PrefOutput::SetSpellcheck(on) => {
                if pref!(self.spellcheck = on) {
                    // Takes effect in already-open composers too: the shared
                    // web context is live.
                    crate::ui::rich_editor::apply_spellcheck();
                }
            }

            PrefOutput::SetSpellcheckLangs(langs) => {
                if pref!(self.spellcheck_langs = langs) {
                    crate::ui::rich_editor::apply_spellcheck();
                    // A composer's right-click menu changes it too (#365):
                    // the Settings window, built once and kept, follows.
                    if let Some(p) = &self.prefs {
                        p.emit(PrefInput::SetSpellcheckLangs(self.spellcheck_langs.clone()));
                    }
                }
            }
            PrefOutput::SetComposeFormat(format) => {
                pref!(self.compose_format = format);
            }
            PrefOutput::SetFollowPlain(on) => {
                pref!(self.follow_plain = on);
            }
            PrefOutput::SetReplyPosition(position) => {
                // A split reply already open stays where it is; the next
                // one opens in the new place.
                pref!(self.reply_position = position);
            }
            PrefOutput::SetSignaturePosition(position) => {
                // Composers already open keep their signature where it is;
                // the next reply or forward opens with the new placement.
                pref!(self.signature_position = position);
            }
            PrefOutput::SetSignatureDashes(on) => {
                pref!(self.signature_dashes = on);
            }
            PrefOutput::SetFoldQuote(on) => {
                pref!(self.fold_quote = on);
            }
            PrefOutput::SetReplyHeader(header) => {
                pref!(self.reply_header = header);
            }
            PrefOutput::SetUndoSend(secs) => {
                // A message already waiting keeps the delay it was sent with.
                pref!(self.undo_send_secs = secs);
            }
            PrefOutput::SetRequestReceipts(on) => {
                // Open composers keep the check they opened with.
                pref!(self.request_receipts = on);
            }
            PrefOutput::SetReturnedReceipts(to) => {
                // Receipts already filed stay where they went.
                pref!(self.returned_receipts = to);
            }
            _ => unreachable!("not routed to on_prefs_compose_pref"),
        }
    }
}
