//! Focus Mode: the chrome that slides away, and back.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::ui::preferences::PrefInput;
use crate::ui::sidebar::SidebarInput;

use super::model::AppModel;

/// Slide a Focus Mode revealer's content away (and fade it, through the
/// `away` class's opacity transition) or bring it back.
fn focus_reveal(r: &gtk::Revealer, shown: bool) {
    if shown {
        r.remove_css_class("away");
    } else {
        r.add_css_class("away");
    }
    r.set_reveal_child(shown);
}

impl AppModel {
    /// Focus Mode changed (the menu, the shortcut or Settings): save it,
    /// and animate whichever parts differ from what is on screen.
    pub(super) fn set_focus_mode(&mut self, focus: config::FocusMode) {
        use config::FocusPart as F;
        let prev = self.focus;
        if prev != focus {
            self.focus = focus;
            config::save_focus_mode(&focus);
            let changed = |part: F| prev.active(part) != focus.active(part);
            if changed(F::ReaderToolbar) || changed(F::ListHeader) {
                self.sync_focus_chrome();
                // Not while the inline composer covers the header: that
                // path hides the ⋯ by hand and restores it on close.
                if !self
                    .reader_compose
                    .as_ref()
                    .is_some_and(|r| r.window.is_none())
                {
                    self.reader_overflow_btn
                        .set_visible(self.reader_overflow_wanted());
                }
            }
            if changed(F::HideAccounts) || changed(F::FoldUnified) {
                self.sidebars_emit(SidebarInput::SetFocus {
                    hide_accounts: focus.active(F::HideAccounts),
                    fold_unified: focus.active(F::FoldUnified),
                    animate: true,
                });
            }
            if changed(F::HideAvatars)
                || changed(F::OnePreviewLine)
                || changed(F::HidePreview)
                || changed(F::HideSubject)
            {
                self.push_list_look(true);
            }
            if changed(F::RailSidebar) {
                // The dots (or counts) are drawn with the rows, so the
                // sidebar is rebuilt before it folds.
                self.rebuild_sidebar();
                self.sync_rail();
            }
            if changed(F::ReaderView) {
                self.push_reader_focus();
            }
        }
        // The menu's check item and Settings stay in step wherever the
        // change came from.
        if self.focus_action.state().and_then(|v| v.get::<bool>()) != Some(focus.enabled) {
            self.focus_action.set_state(&focus.enabled.to_variant());
        }
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::SetFocusMode(focus));
        }
    }

    /// Slide the reader toolbar's buttons and the list header's controls
    /// away or back, as Focus Mode has them. Unmapped widgets (at launch)
    /// jump straight to the state.
    pub(super) fn sync_focus_chrome(&self) {
        use config::FocusPart as F;
        let shown = !self.focus.active(F::ReaderToolbar);
        if let Some(tb) = self.reader_toolbar_widgets.get() {
            for (_, w) in &tb.buttons {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    focus_reveal(r, shown);
                }
            }
            for r in &tb.outbox {
                focus_reveal(r, shown);
            }
        }
        let shown = !self.focus.active(F::ListHeader);
        if let Some(lh) = self.list_header_widgets.get() {
            for r in &lh.revealers {
                focus_reveal(r, shown);
            }
            focus_reveal(&lh.overflow, !shown);
        }
    }

    /// Focus Mode's Reader View came or went: every reader follows, the
    /// message on screen included.
    fn push_reader_focus(&self) {
        let policy = self.effective_reader_default();
        let mode = self.effective_reader_mode();
        self.message_view
            .emit(MessageViewInput::SetReaderDefault(policy));
        self.message_view
            .emit(MessageViewInput::SetReaderMode(mode));
        for p in self.popouts.values() {
            p.controller
                .emit(MessageWindowInput::SetReaderDefault(policy));
            p.controller.emit(MessageWindowInput::SetReaderMode(mode));
        }
    }
}
