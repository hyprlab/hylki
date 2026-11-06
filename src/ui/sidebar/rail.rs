//! The icon rail and Focus Mode: collapsing to icons, and the accounts
//! folding away while Focus Mode is on.

use super::*;

impl Sidebar {
    pub(super) fn on_set_focus(
        &mut self,
        hide_accounts: bool,
        fold_unified: bool,
        animate: bool,
        widgets: &SidebarWidgets,
        sender: ComponentSender<Self>,
    ) {
        let hiding = hide_accounts && !self.focus_hide_accounts;
        let showing = !hide_accounts && self.focus_hide_accounts;
        let folding = fold_unified && !self.focus_fold_unified;
        if !hiding && !showing && fold_unified == self.focus_fold_unified {
            return;
        }
        self.focus_hide_accounts = hide_accounts;
        self.focus_fold_unified = fold_unified;
        // Whatever was opened for the mode alone goes with it.
        self.rail_open.clear();
        self.rail_open_accounts.clear();
        if animate && (hiding || folding) {
            // Slide what goes: the accounts up and away, every open
            // unified row shut. The rebuild that draws the sidebar
            // without them follows once they have gone — the pixels
            // it replaces are then the ones it draws.
            if hiding {
                // SlideDown folding: the block slides up under the
                // unified section, the way a folder list folds.
                if let Some(r) = &self.accounts_revealer {
                    r.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
                    r.set_reveal_child(false);
                }
            }
            if folding {
                self.fold_unified_rows_now();
            }
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(u64::from(crate::ui::FOCUS_ANIM_MS) + 40),
                move || s.input(SidebarInput::FocusSettled),
            );
        } else {
            // Accounts coming back slide in after the rebuild.
            self.reveal_accounts_late = animate && showing;
            self.rebuild(widgets, &sender);
        }
    }

    pub(super) fn on_focus_settled(
        &mut self,
        widgets: &SidebarWidgets,
        sender: ComponentSender<Self>,
    ) {
        self.rebuild(widgets, &sender);
    }

    pub(super) fn on_set_collapsed(
        &mut self,
        collapsed: bool,
        widgets: &SidebarWidgets,
        sender: ComponentSender<Self>,
    ) {
        // Driven by the app's narrow-window breakpoint: same visual
        // change as the user's own toggle, but no CollapsedChanged
        // output — automatic switches must not overwrite the user's
        // persisted preference.
        if self.collapsed != collapsed {
            self.collapsed = collapsed;
            self.rail_open.clear();
            self.rail_open_accounts.clear();
            self.rebuild(widgets, &sender);
        }
    }

    pub(super) fn on_toggle_collapsed(
        &mut self,
        widgets: &SidebarWidgets,
        sender: ComponentSender<Self>,
    ) {
        self.collapsed = !self.collapsed;
        self.rail_open.clear();
        self.rail_open_accounts.clear();
        self.rebuild(widgets, &sender);
        let _ = sender.output(SidebarOutput::CollapsedChanged(self.collapsed));
    }
}
