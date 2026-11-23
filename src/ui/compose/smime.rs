//! S/MIME in the composer (#478): the Send menu's Sign with S/MIME, shown
//! for an account that signs that way and ticked as it says, until the
//! user unticks it for this message. OpenPGP takes precedence: while Sign
//! or Encrypt is on, the check is greyed and unticked, so the menu says
//! what will go out.

use adw::prelude::*;

use super::{Compose, ComposeWidgets};
use crate::i18n::i18n;

impl Compose {
    /// The From identity's account signs with S/MIME.
    fn smime_offered(&self, widgets: &ComposeWidgets) -> bool {
        let idx = widgets.from_row.selected() as usize;
        self.accounts.get(idx).is_some_and(|a| a.smime_sign)
    }

    /// Follow the From account: its setting, unless the user has chosen for
    /// this message. An account that does not sign with S/MIME never does.
    pub(super) fn follow_account_smime(&mut self, widgets: &ComposeWidgets) {
        let offered = self.smime_offered(widgets);
        if !offered || !self.smime_touched {
            self.smime = offered;
        }
        self.show_smime(widgets);
    }

    /// Put the check where the message is. Setting it echoes back as a
    /// [`super::ComposeInput::ToggleSmime`], which `on_toggle_smime`
    /// ignores while the check is not the user's to change.
    pub(super) fn show_smime(&self, widgets: &ComposeWidgets) {
        let check = &self.smime_check;
        let pgp = self.sign || self.encrypt;
        check.set_visible(self.smime_offered(widgets));
        check.set_sensitive(!pgp);
        check.set_tooltip_text(
            pgp.then(|| i18n("Mail sent with OpenPGP is not signed with S/MIME."))
                .as_deref(),
        );
        let shown = self.smime && !pgp;
        if check.is_active() != shown {
            check.set_active(shown);
        }
    }

    /// Handles [`super::ComposeInput::ToggleSmime`].
    pub(super) fn on_toggle_smime(&mut self, on: bool, widgets: &ComposeWidgets) {
        if self.sign || self.encrypt || !self.smime_offered(widgets) || on == self.smime {
            return;
        }
        self.smime = on;
        self.smime_touched = true;
    }
}
