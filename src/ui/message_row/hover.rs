//! The hover actions: the strip of buttons under a row's text while the
//! pointer is on it (Settings → Message List → Hover Actions).

use super::row::Row;
use super::shared::RowLook;
use super::{RowAction, RowData};
use crate::config::HoverAction;
use crate::i18n::i18n;
use adw::prelude::*;

impl Row {
    /// Open and shut the strip as the pointer comes and goes.
    pub(super) fn wire_hover(&self) {
        let motion = &self.w.hover_motion;
        let weak = self.me.clone();
        motion.connect_contains_pointer_notify(move |_| {
            if let Some(row) = weak.upgrade() {
                row.sync_hover_actions();
            }
        });
        self.w.host.add_controller(motion.clone());
    }

    /// Open the strip while the pointer is on a row that has buttons to
    /// show, and not mid-drag or mid-swipe.
    pub(super) fn sync_hover_actions(&self) {
        let visible = {
            let st = self.st.borrow();
            self.w.hover_motion.contains_pointer()
                && !st.dragging
                && !st.swipe_active
                && self.w.hover_box.first_child().is_some()
                && st.item.as_ref().is_some_and(|item| {
                    let data = item.data();
                    data.meta.revealed && data.meta.folded.is_none()
                })
        };
        self.w.hover_reveal.set_reveal_child(visible);
    }

    /// The strip's buttons for the list's setting, and the star's and the
    /// read toggle's state for this message.
    pub(super) fn refresh_hover(&self, data: &RowData, look: &RowLook) {
        // A draft is neither read nor unread, so it has no toggle.
        let wanted: Vec<HoverAction> = look
            .hover_actions
            .iter()
            .copied()
            .filter(|a| !(look.in_drafts && *a == HoverAction::ReadUnread))
            .collect();
        let same = self
            .w
            .hover_buttons
            .borrow()
            .iter()
            .map(|(a, _)| *a)
            .eq(wanted.iter().copied());
        if !same {
            self.build_hover_buttons(&wanted);
        }
        let msg = &data.msg;
        let starred = msg.starred || data.meta.starred;
        let unread = msg.unread || data.meta.unread;
        for (action, button) in self.w.hover_buttons.borrow().iter() {
            match action {
                HoverAction::Star => {
                    set_icon(
                        button,
                        if starred {
                            "starred-symbolic"
                        } else {
                            "hylki-non-starred-symbolic"
                        },
                    );
                    button.set_tooltip_text(Some(&i18n(if starred {
                        "Remove Star"
                    } else {
                        "Star"
                    })));
                    if starred {
                        button.add_css_class("star-active");
                    } else {
                        button.remove_css_class("star-active");
                    }
                }
                HoverAction::ReadUnread => {
                    set_icon(
                        button,
                        if unread {
                            "hylki-mail-read-symbolic"
                        } else {
                            "mail-unread-symbolic"
                        },
                    );
                    button.set_tooltip_text(Some(&i18n(if unread {
                        "Mark as Read"
                    } else {
                        "Mark as Unread"
                    })));
                }
                HoverAction::Spam => {
                    // In Junk the way back is Not Spam (#168).
                    set_icon(
                        button,
                        if look.in_junk {
                            "mail-mark-notjunk-symbolic"
                        } else {
                            "mail-mark-junk-symbolic"
                        },
                    );
                    button.set_tooltip_text(Some(&i18n(if look.in_junk {
                        "Not Spam"
                    } else {
                        "Mark as Spam"
                    })));
                }
                _ => {}
            }
        }
        self.sync_hover_actions();
    }

    fn build_hover_buttons(&self, actions: &[HoverAction]) {
        let bx = &self.w.hover_box;
        while let Some(child) = bx.first_child() {
            bx.remove(&child);
        }
        let mut buttons = Vec::with_capacity(actions.len());
        for &action in actions {
            let button = gtk::Button::from_icon_name(action.icon());
            button.add_css_class("flat");
            button.add_css_class("circular");
            // A click acts on the row without taking the list's focus.
            button.set_focusable(false);
            button.set_focus_on_click(false);
            button.set_tooltip_text(Some(&i18n(action.label())));
            let weak = self.me.clone();
            button.connect_clicked(move |_| {
                if let Some(row) = weak.upgrade() {
                    row.act_hover(action);
                }
            });
            bx.append(&button);
            buttons.push((action, button));
        }
        *self.w.hover_buttons.borrow_mut() = buttons;
    }

    /// A strip button's action, as the row's menu would send it.
    fn act_hover(&self, action: HoverAction) {
        let in_junk = self
            .shared()
            .is_some_and(|shared| shared.look.borrow().in_junk);
        self.act(match action {
            HoverAction::Star => RowAction::ToggleStar,
            HoverAction::ReadUnread => RowAction::ToggleRead,
            HoverAction::Archive => RowAction::Archive,
            HoverAction::Delete => RowAction::Delete,
            HoverAction::Spam if in_junk => RowAction::NotSpam,
            HoverAction::Spam => RowAction::Spam,
            HoverAction::Reply => RowAction::Reply,
            HoverAction::ReplyAll => RowAction::ReplyAll,
            HoverAction::Forward => RowAction::Forward,
        });
    }
}

/// Change a button's icon only when it differs: a row refreshes often.
fn set_icon(button: &gtk::Button, icon: &str) {
    if button.icon_name().as_deref() != Some(icon) {
        button.set_icon_name(icon);
    }
}
