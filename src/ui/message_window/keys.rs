//! The pop-out's keyboard: the same accelerators and single keys as the
//! main window, read through the same maps.

use super::types::{MessageWindowInput, MessageWindowOutput};
use super::MessageWindow;
use crate::ui::keymap::Shortcut;
use crate::ui::message_view::MessageViewInput;
use adw::prelude::*;
use relm4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

impl MessageWindow {
    /// A single-key shortcut (#392). The window does what concerns its own
    /// message and reader, as its toolbar does; the rest goes to the app.
    pub(super) fn run_key(
        &mut self,
        action: Shortcut,
        sender: &ComponentSender<Self>,
        root: &adw::Window,
    ) {
        let conversation = self.thread.len() > 1;
        let input = |msg| sender.input_sender().emit(msg);
        match action {
            Shortcut::BackToList => root.close(),
            Shortcut::OpenMessage => self.view.emit(MessageViewInput::FocusBody),
            Shortcut::NextInThread if conversation => self.view.emit(MessageViewInput::StepCard(1)),
            Shortcut::PrevInThread if conversation => {
                self.view.emit(MessageViewInput::StepCard(-1))
            }
            Shortcut::FoldAll(folded) => self.view.emit(MessageViewInput::FoldAll(folded)),
            Shortcut::Star => input(MessageWindowInput::ToggleStar),
            Shortcut::ToggleRead => input(MessageWindowInput::ToggleRead),
            Shortcut::Delete => input(MessageWindowInput::Delete),
            Shortcut::Spam => input(MessageWindowInput::Spam),
            Shortcut::MoveTo => input(MessageWindowInput::MoveTo),
            Shortcut::Snooze => input(MessageWindowInput::SnoozeMenu),
            Shortcut::Label => input(MessageWindowInput::TagMenu),
            Shortcut::Tag(n) => {
                if let Some(tag) = self.tags.get(usize::from(n).saturating_sub(1)) {
                    let _ = sender.output(MessageWindowOutput::SetTag {
                        message: Box::new(self.msg.clone()),
                        keyword: tag.keyword.clone(),
                        add: !self.msg.has_tag(tag),
                    });
                }
            }
            Shortcut::ClearTags => {
                for tag in self.tags.iter().filter(|t| self.msg.has_tag(t)) {
                    let _ = sender.output(MessageWindowOutput::SetTag {
                        message: Box::new(self.msg.clone()),
                        keyword: tag.keyword.clone(),
                        add: false,
                    });
                }
            }
            Shortcut::Reply
            | Shortcut::ReplyAll
            | Shortcut::Forward
            | Shortcut::InWindow(_)
            | Shortcut::Compose
            | Shortcut::Undo
            | Shortcut::Shortcuts
            | Shortcut::Refresh
            | Shortcut::DeleteForever
            | Shortcut::MarkRead(_)
            | Shortcut::MarkUnreadFrom
            | Shortcut::Archive
            | Shortcut::ArchiveAndStep(_) => {
                let _ = sender.output(MessageWindowOutput::Shortcut {
                    action,
                    message: Box::new(self.msg.clone()),
                    thread: self.thread.clone(),
                });
                // The conversation has left this folder, or, with Gmail's
                // keys, been put back unread: the window has done its job,
                // as a conversation closes in the main window.
                let unread = matches!(action, Shortcut::MarkRead(false) | Shortcut::MarkUnreadFrom);
                if matches!(action, Shortcut::Archive | Shortcut::ArchiveAndStep(_))
                    || (unread && self.gmail_keys.get())
                {
                    root.close();
                }
            }
            // The list's keys, and Gmail's g and * sequences, have nothing
            // to act on in a window of one conversation.
            _ => {}
        }
    }
}
/// Ctrl+P prints and Ctrl+F finds, matching the main window. This
/// window has no menu bar to hang an action off, so the accelerators
/// are wired directly. The single keys are the main window's too
/// (#392), read through the same maps, so the two cannot drift.
pub(super) fn key_controller(
    sender: &ComponentSender<MessageWindow>,
    window: &adw::Window,
    single_key: Rc<Cell<bool>>,
    gmail: Rc<Cell<bool>>,
) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    let s = sender.clone();
    let window = window.clone();
    // When Gmail's g or * came: the key after it names a folder or a
    // selection in the main window, and is no action here.
    let sequence: std::rc::Rc<std::cell::Cell<Option<std::time::Instant>>> = Default::default();
    keys.connect_key_pressed(move |_, keyval, _, state| {
        use gtk::gdk::Key;
        let keyval = crate::ui::keymap::without_caps_lock(keyval, state);
        let key = |action| {
            s.input_sender().emit(MessageWindowInput::Key(action));
            gtk::glib::Propagation::Stop
        };
        let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
            // Shift+P is the preview; the keyval arrives capitalised.
            if keyval == Key::P {
                s.input_sender().emit(MessageWindowInput::PrintPreview);
                return gtk::glib::Propagation::Stop;
            }
            if keyval == Key::p {
                s.input_sender().emit(MessageWindowInput::Print);
                return gtk::glib::Propagation::Stop;
            }
            if keyval == Key::f {
                s.input_sender().emit(MessageWindowInput::Find);
                return gtk::glib::Propagation::Stop;
            }
            if keyval == Key::z && !shift && !crate::ui::keymap::focus_is_text(&window) {
                return key(Shortcut::Undo);
            }
            return gtk::glib::Propagation::Proceed;
        }
        if state.contains(gtk::gdk::ModifierType::ALT_MASK)
            || crate::ui::keymap::focus_is_text(&window)
        {
            return gtk::glib::Propagation::Proceed;
        }
        // Escape closes the window, as it backs out of a message in
        // the main one, with or without the single keys.
        if keyval == Key::Escape {
            return key(Shortcut::BackToList);
        }
        // Delete and Shift+Delete too, as in the list (#486).
        if matches!(keyval, Key::Delete | Key::KP_Delete) {
            return key(if shift {
                Shortcut::DeleteForever
            } else {
                Shortcut::Delete
            });
        }
        if !single_key.get() || matches!(keyval, Key::Shift_L | Key::Shift_R) {
            return gtk::glib::Propagation::Proceed;
        }
        if crate::ui::keymap::focus_takes_keys(&window) && crate::ui::keymap::reader_keeps(keyval) {
            return gtk::glib::Propagation::Proceed;
        }
        if gmail.get() {
            let after = sequence
                .take()
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(1500));
            if after {
                return gtk::glib::Propagation::Stop;
            }
            if matches!(keyval, Key::g | Key::asterisk) {
                sequence.set(Some(std::time::Instant::now()));
                return gtk::glib::Propagation::Stop;
            }
        }
        let action = if gmail.get() {
            crate::ui::keymap::gmail_shortcut_for(keyval, shift)
        } else {
            crate::ui::keymap::shortcut_for(keyval, shift)
        };
        match action {
            Some(action) => key(action),
            None => gtk::glib::Propagation::Proceed,
        }
    });
    keys
}
