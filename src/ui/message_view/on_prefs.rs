//! The reader's preferences, set from the app: each stores the value and
//! renders or patches what it changes.

use super::MessageView;
use relm4::prelude::*;
use webkit6::prelude::WebViewExt;

impl MessageView {
    pub(super) fn on_set_fold_messages(&mut self, fold: crate::config::FoldMessages) {
        if self.fold_messages != fold {
            self.fold_messages = fold;
            self.folds.clear();
            self.render();
        }
    }

    pub(super) fn on_set_fold_look(&mut self, preview_lines: u32) {
        let preview_lines = preview_lines.min(3);
        if self.fold_preview_lines != preview_lines {
            self.fold_preview_lines = preview_lines;
            self.render();
        }
    }

    pub(super) fn on_set_pgp_labels(&mut self, on: bool) {
        self.pgp_labels = on;
        let js = format!(
            "document.body&&document.body.toggleAttribute('data-hylki-pgpicons',{})",
            !on
        );
        self.run_js(&js);
    }

    pub(super) fn on_set_card_attachments_shown(&mut self, on: bool) {
        if self.card_atts_shown != on {
            self.card_atts_shown = on;
            if self.current.is_some() && !self.loading {
                self.render();
            }
        }
    }

    pub(super) fn on_set_attachment_drawer(&mut self, on: bool) {
        if self.drawer_on != on {
            self.drawer_on = on;
            if self.current.is_some() && !self.loading {
                self.render();
            }
        }
    }

    pub(super) fn on_set_content_theme(&mut self, o: Option<bool>) {
        if self.content_dark != o {
            self.content_dark = o;
            let dark = self.effective_dark();
            self.apply_webview_bg(dark);
            if self.current.is_some() && !self.loading {
                self.render();
            }
        }
    }

    pub(super) fn on_set_reader_style(&mut self, style: crate::config::ReaderStyle) {
        if self.reader_style != style {
            self.reader_style = style;
            if self.current.is_some() && !self.loading {
                self.render();
            }
        }
    }

    pub(super) fn on_set_message_flipped(&mut self, on: bool) {
        if self.message_flipped() == on {
            return;
        }
        let Some(key) = self.current.as_ref().map(|m| (m.account_id, m.id)) else {
            return;
        };
        // Off forgets the choice: the message follows the theme again.
        if on {
            self.theme_picks.insert(key, !self.theme_dark());
        } else {
            self.theme_picks.remove(&key);
        }
        let dark = self.effective_dark();
        self.apply_webview_bg(dark);
        if !self.loading {
            self.render();
        }
    }

    pub(super) fn on_set_zoom(&mut self, percent: u32) {
        if self.zoom != percent {
            self.zoom = percent;
            // The frames remeasure at the new size; the heights
            // remembered for the old one would only make them lurch.
            self.frame_heights.clear();
            if self.current.is_some() && !self.loading {
                let js = format!(
                    "if(window.vfZoom)window.vfZoom({});",
                    percent as f64 / 100.0
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            }
        }
    }

    pub(super) fn on_set_reader_default(&mut self, policy: crate::config::ReaderDefault) {
        self.reader_default = policy;
        if let Some(on) = policy.starts_on() {
            self.set_reader_mode(on);
        }
    }

    pub(super) fn on_set_unsubscribed(&mut self, lists: std::collections::HashMap<String, i64>) {
        self.unsubscribed = lists;
        let keys: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        for (account_id, id) in keys {
            self.patch_unsub(account_id, id);
        }
    }

    pub(super) fn on_set_identities(
        &mut self,
        identities: std::collections::HashMap<u32, Vec<String>>,
    ) {
        if self.identities == identities {
            return;
        }
        self.identities = identities;
        let keys: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        for (account_id, id) in keys {
            self.patch_invite(account_id, id);
        }
    }

    pub(super) fn on_set_invite_answers(
        &mut self,
        answers: std::collections::HashMap<String, (String, i64)>,
    ) {
        self.invite_answers = answers;
        let keys: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        for (account_id, id) in keys {
            self.patch_invite(account_id, id);
        }
    }

    pub(super) fn on_set_read_mark(&mut self, policy: crate::config::ReadMark) {
        if self.read_mark != policy {
            self.read_mark = policy;
            if self.current.is_some() && !self.loading {
                self.render();
            }
        }
    }

    pub(super) fn on_set_tags(&mut self, tags: Vec<crate::config::Tag>) {
        if self.tags != tags {
            self.tags = tags;
            *self.header_tags_rendered.borrow_mut() = None;
            if !self.thread.is_empty() {
                // The chips are baked into the document: draw it again.
                self.shown_fingerprint = None;
                self.render();
            }
        }
    }

    pub(super) fn on_set_template_folders(
        &mut self,
        folders: std::collections::HashSet<(u32, u32)>,
    ) {
        if self.template_folders != folders {
            let shown = |set: &std::collections::HashSet<(u32, u32)>| {
                self.thread
                    .iter()
                    .any(|m| set.contains(&(m.account_id, m.folder_id)))
            };
            let redraw = shown(&self.template_folders) || shown(&folders);
            self.template_folders = folders;
            if redraw {
                self.render();
            }
        }
    }
}

impl MessageView {
    /// Reader View on or off, re-rendering what is on screen if it changed.
    pub(super) fn set_reader_mode(&mut self, on: bool) {
        if self.reader_mode == on {
            return;
        }
        self.reader_mode = on;
        // A frame measures differently in each view; the heights remembered
        // for the other one would only make the cards lurch on the way to
        // their real size.
        self.frame_heights.clear();
        if self.current.is_some() && !self.loading {
            self.render();
        }
    }
}
