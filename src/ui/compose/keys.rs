//! The composer's keyboard shortcuts.

use super::messages::ComposeInput;
use super::types::Field;
use super::Compose;
use crate::ui::rich_editor;
use adw::prelude::*;
use relm4::prelude::*;

/// Ctrl+Shift+A, T, C or B as what it asks the composer for.
pub(crate) fn composer_key(
    keyval: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
) -> Option<ComposeInput> {
    use gtk::gdk::{Key, ModifierType as M};
    if state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK)
        != M::CONTROL_MASK | M::SHIFT_MASK
    {
        return None;
    }
    match keyval.to_lower() {
        Key::a => Some(ComposeInput::AttachFiles),
        Key::t => Some(ComposeInput::FocusField(Field::To)),
        Key::c => Some(ComposeInput::FocusField(Field::Cc)),
        Key::b => Some(ComposeInput::FocusField(Field::Bcc)),
        _ => None,
    }
}

/// Ctrl+ (or Ctrl+=, for layouts where + needs Shift), Ctrl- and Ctrl+0,
/// keypad included, as the zoom step they ask for: the reader's keys.
pub(super) fn zoom_key(keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> Option<i8> {
    use gtk::gdk::Key;
    if !state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
        || state.contains(gtk::gdk::ModifierType::ALT_MASK)
    {
        return None;
    }
    // By value, as the suggestion keys are.
    if [Key::plus, Key::equal, Key::KP_Add].contains(&keyval) {
        Some(1)
    } else if [Key::minus, Key::KP_Subtract].contains(&keyval) {
        Some(-1)
    } else if [Key::_0, Key::KP_0].contains(&keyval) {
        Some(0)
    } else {
        None
    }
}

/// Whether keyboard focus is in a one-line text field of the composer — an
/// address row or the subject. Those keep their own native undo; everything
/// else in the composer answers to its history. The body is a WebView, not a
/// `GtkEditable`, so it never matches here.
fn focus_is_entry(root: &impl IsA<gtk::Widget>) -> bool {
    let mut node = root
        .as_ref()
        .root()
        .and_downcast::<gtk::Window>()
        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
    while let Some(widget) = node {
        if widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>() {
            return true;
        }
        node = widget.parent();
    }
    false
}

/// Tab or Shift+Tab, with no Ctrl or Alt, as the way it moves focus.
fn tab_direction(
    keyval: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
) -> Option<gtk::DirectionType> {
    use gtk::gdk::{Key, ModifierType as M};
    if state.intersects(M::CONTROL_MASK | M::ALT_MASK | M::SUPER_MASK) {
        return None;
    }
    match keyval {
        Key::ISO_Left_Tab => Some(gtk::DirectionType::TabBackward),
        Key::Tab | Key::KP_Tab if state.contains(M::SHIFT_MASK) => {
            Some(gtk::DirectionType::TabBackward)
        }
        Key::Tab | Key::KP_Tab => Some(gtk::DirectionType::TabForward),
        _ => None,
    }
}

/// Move focus one step within `root`, coming round to its other end past
/// the first or last thing that takes focus, as a window does.
fn cycle_focus(root: &gtk::Widget, direction: gtk::DirectionType) {
    if root.child_focus(direction) {
        return;
    }
    // A move starts from the focus child of each level, so the chain is
    // cleared to start again from the edge.
    let mut node = Some(root.clone());
    while let Some(widget) = node {
        node = widget.focus_child();
        widget.set_focus_child(None::<&gtk::Widget>);
    }
    root.child_focus(direction);
}

impl Compose {
    pub(super) fn install_keys(&self, root: &adw::BreakpointBin, sender: &ComponentSender<Self>) {
        // Drive the suggestion list from a single capture-phase key handler on
        // the window — the toplevel sees every key first, regardless of focus.
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let s = sender.clone();
        let open = self.completion_open.clone();
        let editor = self.editor.clone();
        // Weak: the root owns this controller, so a strong root here would
        // be a cycle that kept a closed composer's whole widget tree alive,
        // its editor's WebKitWebProcess included (#221).
        let key_root = root.downgrade();
        key.connect_key_pressed(move |_, keyval, _, state| {
            use gtk::glib::Propagation;
            let Some(key_root) = key_root.upgrade() else {
                return Propagation::Proceed;
            };
            // Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y run the composer's own history
            // (#200): the body's typing and formatting with the attachments
            // in the same order. Caught here, in the capture phase above the
            // editor, so the editor's own handler for the bare body never
            // sees them and the attachments stay in the order.
            //
            // The address and subject rows are the exception: GTK gives every
            // GtkText an undo history of its own, and for a single line that
            // is the one the user means.
            if let Some(redo) = rich_editor::history_key(keyval, state) {
                if !focus_is_entry(&key_root) {
                    s.input_sender().emit(ComposeInput::History { redo });
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Ctrl+V pastes per the "Paste as plain text" preference, read
            // here so a settings change applies to composers already open.
            // Only over the body: the address and subject entries are plain
            // text by nature, and the focus guard leaves their Ctrl+V alone.
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && keyval == gtk::gdk::Key::v
                && editor.has_focus()
            {
                editor.paste(!crate::config::load_privacy().paste_plain);
                return Propagation::Stop;
            }
            // Ctrl+ / Ctrl- / Ctrl+0 zoom the body, as they zoom the reader.
            // This is a composer window's; inline, the main window's own
            // keys get there first and pass them on (app/shortcuts.rs).
            if let Some(step) = zoom_key(keyval, state) {
                s.input_sender().emit(ComposeInput::Zoom(step));
                return Propagation::Stop;
            }
            // Ctrl+Enter sends (#238), as it does in Gmail, Apple Mail and
            // Thunderbird. Ahead of the suggestion list's own Enter, so a
            // press with the list open never both accepts a name and sends.
            // Send itself refuses an unaddressed message, so the shortcut
            // cannot post what the button would not.
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && (keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter)
            {
                s.input_sender().emit(ComposeInput::Send);
                return Propagation::Stop;
            }
            // Thunderbird's keys (#490, #491): Ctrl+Shift+A attaches files,
            // Ctrl+Shift+T, C and B go to To, Cc and Bcc. A composer window
            // gets them here; inline, the same keys are the main window's
            // first, and app/shortcuts.rs passes them on.
            if let Some(input) = composer_key(keyval, state) {
                s.input_sender().emit(input);
                return Propagation::Stop;
            }
            // Tab and Shift+Tab go round the composer and nowhere else.
            // Left to the window, Tab off the body went on into the main
            // window's sidebar and list, never back to To. The HTML and
            // Markdown source keeps Tab, which indents there.
            if let Some(direction) = tab_direction(keyval, state) {
                if editor.source_kind().is_some() && editor.has_focus() {
                    return Propagation::Proceed;
                }
                cycle_focus(key_root.upcast_ref(), direction);
                // Focus on the view alone leaves the document without a
                // caret, and typing would go nowhere (#266).
                if editor.has_focus() {
                    editor.grab_focus();
                }
                return Propagation::Stop;
            }
            if !open.get() {
                // Escape backs out of the whole composer, the same as Cancel:
                // an untouched reply goes at once, an edited one asks first
                // (#290). Only once the suggestion list is closed, which
                // Escape dismisses first (below), so one press never does both.
                if keyval == gtk::gdk::Key::Escape {
                    s.input_sender().emit(ComposeInput::Cancel);
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Compare by value (the const-as-pattern match wasn't matching).
            if keyval == gtk::gdk::Key::Down {
                s.input_sender().emit(ComposeInput::CompletionMove(1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Up {
                s.input_sender().emit(ComposeInput::CompletionMove(-1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter {
                s.input_sender().emit(ComposeInput::CompletionAccept);
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Escape {
                s.input_sender().emit(ComposeInput::CompletionClose);
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
        root.add_controller(key);
    }
}
