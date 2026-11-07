//! Short saved phrases, inserted at the caret.

use adw::prelude::*;
use relm4::prelude::*;

use super::messages::{ComposeInput, ComposeOutput};
use super::Compose;
use crate::i18n::i18n;
use crate::ui::context_menu::{show_context_menu_with_header, MenuEntry};

impl Compose {
    /// List the saved phrases under the quick-insert button.
    pub(super) fn on_quick_inserts_menu(&self, sender: &ComponentSender<Self>) {
        let mut entries: Vec<MenuEntry> = self
            .quick_inserts
            .iter()
            .filter(|phrase| !phrase.trim().is_empty())
            .map(|phrase| {
                let phrase = phrase.trim().to_string();
                let shown = phrase.clone();
                let s = sender.clone();
                MenuEntry::new(&shown, move || {
                    s.input_sender()
                        .emit(ComposeInput::QuickInsert(phrase.clone()))
                })
                .icon("edit-paste-symbolic")
                .tooltip(&shown)
            })
            .collect();
        if entries.is_empty() {
            entries.push(MenuEntry::new(i18n("No Quick Inserts"), || {}).enabled(false));
        }
        let s = sender.clone();
        let configure = MenuEntry::new(i18n("Configure Quick Inserts…"), move || {
            let _ = s.output(ComposeOutput::OpenQuickInsertSettings);
        });
        let btn = &self.quick_insert_btn;
        show_context_menu_with_header(
            btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            Some(&i18n("Quick Inserts")),
            vec![entries, vec![configure]],
        );
    }

    /// Put a phrase where the caret is and keep the user writing there.
    pub(super) fn on_quick_insert(&self, phrase: String) {
        let phrase = phrase.trim();
        if phrase.is_empty() {
            return;
        }
        self.editor.insert_at_caret(phrase);
        self.editor.grab_focus();
    }
}
