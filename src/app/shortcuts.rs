//! Running a keyboard shortcut in the main window or a pop-out.

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n;
use crate::models::{FolderKind, Message};
use crate::ui::compose::ComposeInput;
use crate::ui::contacts_page::ContactsPageInput;
use crate::ui::message_list::{BulkAction, MessageListInput, RowAction};
use crate::ui::message_view::MessageViewInput;

use super::flags::read_label;
use crate::ui::keymap::{
    focus_in_compose, focus_is_text, focus_takes_keys, gmail_sequence, gmail_shortcut_for,
    reader_keeps, shortcut_for, without_caps_lock, GoTarget, Shortcut, WindowCompose,
};

use super::keymap::ctrl_shortcut_for;
use super::model::AppModel;
use super::msg::AppMsg;
use super::types::UnifiedView;
use super::undo::{FlagChange, FlagKind, FlagRef, UndoStep};

impl AppModel {
    /// Carry out a single-key shortcut.
    fn run_shortcut(&mut self, action: Shortcut, sender: &ComponentSender<Self>) {
        // Contacts covers the list and the reader: a key there must not
        // act on mail out of sight, Delete outside its list above all.
        if self.showing_contacts && action.acts_on_selection() {
            return;
        }
        // Checking rows with none checked: say why the key did nothing
        // (#392).
        if self.checked == Some(0) && action.acts_on_selection() {
            self.notice(i18n("Select a conversation first."));
            return;
        }
        match action {
            Shortcut::NextMessage => self.message_list.emit(MessageListInput::MoveSelection(1)),
            Shortcut::PrevMessage => self.message_list.emit(MessageListInput::MoveSelection(-1)),
            Shortcut::ToggleSelect => self.message_list.emit(MessageListInput::ToggleSelection),
            Shortcut::BackToList => {
                // Gmail's u and Esc close the conversation, back to the list
                // with nothing open (#392); the classic keys only move the
                // keyboard there. A draft being written in the reader stays.
                if self.gmail_on() && self.reader_compose.is_none() {
                    self.message_list.emit(MessageListInput::CloseOpen);
                } else {
                    self.message_list.emit(MessageListInput::FocusList);
                }
            }
            Shortcut::Search if self.showing_contacts => {
                self.contacts_page.emit(ContactsPageInput::FocusSearch)
            }
            Shortcut::Search => self.message_list.emit(MessageListInput::FocusSearch),
            // The open message's own view, not the reader's first page,
            // which is the "No message selected" one and takes no focus.
            Shortcut::OpenMessage => self.message_view.emit(MessageViewInput::FocusBody),
            Shortcut::MoveCursor(delta) => {
                // In the reader, Gmail's j and k open the next or previous
                // conversation; in the list they only move the cursor (#392).
                if self.current.is_some() && self.reader_has_focus() {
                    self.message_list
                        .emit(MessageListInput::StepConversation(delta));
                } else {
                    self.message_list.emit(MessageListInput::MoveCursor(delta));
                }
            }
            Shortcut::OpenCursor => self.message_list.emit(MessageListInput::OpenCursor),
            Shortcut::MarkRead(read) => self.mark_read_by_key(read),
            Shortcut::Undo => sender.input(AppMsg::Undo),
            Shortcut::MoveTo => sender.input(AppMsg::MoveToMenu),
            Shortcut::Snooze => sender.input(AppMsg::SnoozeMenu),
            Shortcut::Label => sender.input(AppMsg::ReaderTagMenu),
            Shortcut::SelectAll => self.message_list.emit(MessageListInput::SelectAllShown),
            Shortcut::SelectNone => self.message_list.emit(MessageListInput::ClearSelection),
            Shortcut::SelectWhere(filter) => self
                .message_list
                .emit(MessageListInput::SelectWhere(filter)),
            Shortcut::ArchiveAndStep(delta) => {
                // The list goes on to the neighbour once the conversation's
                // row leaves it. On Gmail, from a label, archiving leaves the
                // row where it is, so the step is taken here instead.
                let stays = self
                    .current
                    .as_ref()
                    .is_some_and(|m| self.archives_in_place(m));
                self.message_list.emit(if stays {
                    MessageListInput::StepConversation(delta)
                } else {
                    MessageListInput::SetNavDirection(delta)
                });
                self.run_shortcut(Shortcut::Archive, sender);
            }
            Shortcut::InWindow(what) => sender.input(AppMsg::ComposeInWindow(what)),
            Shortcut::Refresh => sender.input(AppMsg::Refresh),
            Shortcut::GoTo(GoTarget::Contacts) => sender.input(AppMsg::OpenContacts),
            Shortcut::GoTo(GoTarget::Kind(kind)) => {
                // The merged view when the sidebar lists one, else the
                // account's own folder of that kind.
                let merged = match kind {
                    FolderKind::Inbox => self.unified_inboxes_shown(),
                    _ => self.unified_kinds_listed().has(kind),
                };
                if merged {
                    self.open_unified(UnifiedView::Kind(kind), sender);
                } else {
                    sender.input(AppMsg::ShowcaseFolder {
                        kind,
                        account: None,
                    });
                }
            }
            Shortcut::NextInThread => self.step_thread(1),
            Shortcut::PrevInThread => self.step_thread(-1),
            Shortcut::Reply => sender.input(AppMsg::Reply),
            Shortcut::ReplyAll => sender.input(AppMsg::ReplyAll),
            Shortcut::Forward => sender.input(AppMsg::Forward),
            Shortcut::Archive => {
                // Gmail's e archives the conversation even with one of its
                // cards stepped to (#392); the toolbar's Archive, and the
                // classic keys, leave a picked card on its own.
                if self.gmail_keys.get() && self.acts_on_conversation(true) {
                    let conversation = self.conversation_to_archive();
                    if !conversation.is_empty() {
                        self.apply_bulk_move(BulkAction::Archive, conversation);
                        return;
                    }
                }
                sender.input(AppMsg::Archive);
            }
            Shortcut::MarkUnreadFrom => {
                self.mark_unread_from();
                self.close_after_unread();
            }
            Shortcut::FoldAll(folded) => {
                if self.current_thread.len() > 1 {
                    self.message_view.emit(MessageViewInput::FoldAll(folded));
                }
            }
            Shortcut::Delete => sender.input(AppMsg::Delete),
            Shortcut::DeleteForever => sender.input(AppMsg::DeleteForever),
            Shortcut::Spam => sender.input(AppMsg::MarkSpam),
            Shortcut::Star => sender.input(AppMsg::ToggleStar),
            Shortcut::ToggleRead => {
                if self.many_selected() {
                    sender.input(AppMsg::ToggleReadCurrent);
                } else if let Some(m) = self.current.clone() {
                    let read = m.unread;
                    self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
                }
            }
            Shortcut::Compose => sender.input(AppMsg::Compose),
            Shortcut::Shortcuts => self.show_shortcuts(),
            Shortcut::Tag(n) => {
                // Settings order, first nine. A number past the list does
                // nothing rather than something surprising.
                if let Some(tag) = self.tags.get(usize::from(n).saturating_sub(1)) {
                    sender.input(AppMsg::ToggleTagCurrent(tag.keyword.clone()));
                }
            }
            Shortcut::ClearTags => self.clear_tags_on_target(),
        }
    }

    /// Marks read or unread by key: the checked rows, else the selection,
    /// else the whole conversation under Gmail's keys (#392), else the
    /// open message.
    fn mark_read_by_key(&mut self, read: bool) {
        if self.rows_selected() {
            self.message_list.emit(MessageListInput::Bulk(if read {
                BulkAction::MarkRead
            } else {
                BulkAction::MarkUnread
            }));
            return;
        }
        let mut targets = self.multi_selection();
        if targets.is_empty() && self.acts_on_conversation(self.gmail_keys.get()) {
            // The whole conversation, as Gmail marks it (#392).
            targets = if read {
                self.current_thread.clone()
            } else {
                self.conversation_to_mark_unread()
            };
        }
        if targets.is_empty() {
            targets.extend(self.reply_target());
        }
        if !read {
            self.hold_unread(&targets);
        }
        targets.retain(|m| m.unread == read);
        if !targets.is_empty() {
            self.user_set_flags(read_label(read), &targets, FlagKind::Read, read);
        }
        // Gmail's Shift+U puts the conversation back in the list,
        // unread, rather than leave it open to be read again.
        if !read {
            self.close_after_unread();
        }
    }

    /// Takes the configured tags off the selected messages, or the open
    /// one, as one undo step per account.
    fn clear_tags_on_target(&mut self) {
        // Only configured tags come off: a message's other keywords
        // ($Forwarded, a client's own flags) are not ours to drop.
        // Every selected message loses them when several are
        // selected (#313). Re-read the message between removals,
        // since set_tag patches the copy the next call will read.
        let targets = if self.many_selected() {
            self.multi_selection()
        } else {
            self.reply_target().into_iter().collect()
        };
        let mut changes: HashMap<u32, Vec<FlagChange>> = HashMap::new();
        for target in targets {
            let keywords: Vec<String> = self
                .tags
                .iter()
                .filter(|t| target.has_tag(t))
                .map(|t| t.keyword.clone())
                .collect();
            for keyword in keywords {
                let m = self
                    .shown_copy(target.account_id, target.id)
                    .unwrap_or_else(|| target.clone());
                changes.entry(m.account_id).or_default().push(FlagChange {
                    kind: FlagKind::Tag(keyword.clone()),
                    // The step is the reversal: undo puts them back.
                    value: true,
                    items: vec![FlagRef {
                        folder_id: m.folder_id,
                        uid: m.uid,
                        id: m.id,
                    }],
                });
                self.set_tag(&m, &keyword, false);
            }
        }
        // One entry per account, so a single Ctrl+Z restores the
        // whole set rather than giving back one tag per press.
        for (account_id, changes) in changes {
            self.push_undo(account_id, i18n("Remove Tags"), UndoStep::Flags(changes));
        }
    }

    /// Move to the next (or previous) message of the open conversation:
    /// its card in the reader, opened and scrolled to, which is what actions
    /// then apply to. Selecting the message in the list instead landed on
    /// the conversation's row again, so the keys went nowhere (#392).
    fn step_thread(&self, delta: i32) {
        if self.current.is_none() {
            return;
        }
        if self.current_thread.len() > 1 {
            self.message_view.emit(MessageViewInput::StepCard(delta));
        } else {
            // Not in a conversation: moving through the list is what someone
            // pressing "next" almost certainly meant.
            self.message_list
                .emit(MessageListInput::MoveSelection(delta));
        }
    }

    /// The keys a message window hands over (#392): what reaches past its
    /// own message, to the rest of its conversation or to the app. Gmail's
    /// keys take the whole conversation, as they do in the main window.
    pub(super) fn run_popout_shortcut(
        &mut self,
        action: Shortcut,
        m: Message,
        thread: Vec<Message>,
        sender: &ComponentSender<Self>,
    ) {
        let whole = self.gmail_keys.get() && thread.len() > 1;
        // The window's copies are as it opened; the flags may have moved.
        let thread: Vec<Message> = thread
            .into_iter()
            .map(|t| self.shown_copy(t.account_id, t.id).unwrap_or(t))
            .collect();
        let row = |action| AppMsg::RowAction {
            action,
            message: Box::new(m.clone()),
            conversation: thread.clone(),
        };
        match action {
            Shortcut::Reply | Shortcut::InWindow(WindowCompose::Reply) => {
                sender.input(row(RowAction::Reply))
            }
            Shortcut::ReplyAll | Shortcut::InWindow(WindowCompose::ReplyAll) => {
                sender.input(row(RowAction::ReplyAll))
            }
            Shortcut::Forward | Shortcut::InWindow(WindowCompose::Forward) => {
                sender.input(row(RowAction::Forward))
            }
            Shortcut::Archive | Shortcut::ArchiveAndStep(_) => {
                if whole {
                    // What the list row stands for: the members filed with
                    // the window's message, not a reply kept in Sent.
                    let members = thread
                        .into_iter()
                        .filter(|t| (t.account_id, t.folder_id) == (m.account_id, m.folder_id))
                        .collect();
                    self.apply_bulk_move(BulkAction::Archive, members);
                } else {
                    self.move_to(m, FolderKind::Archive);
                }
            }
            Shortcut::MarkRead(read) => {
                let mut targets = match (whole, read) {
                    (false, _) => vec![self.shown_copy(m.account_id, m.id).unwrap_or(m)],
                    (true, true) => thread,
                    (true, false) => thread
                        .into_iter()
                        .filter(|t| {
                            !matches!(
                                self.folder_kind(t.account_id, t.folder_id),
                                Some(FolderKind::Sent | FolderKind::Drafts)
                            )
                        })
                        .collect(),
                };
                targets.retain(|t| t.unread == read);
                if !targets.is_empty() {
                    self.user_set_flags(read_label(read), &targets, FlagKind::Read, read);
                }
            }
            Shortcut::MarkUnreadFrom => {
                // The window picks no card, so the message read from is the
                // newest, as in the main window with none picked.
                let newest = thread
                    .into_iter()
                    .max_by_key(|t| (t.timestamp, t.uid))
                    .unwrap_or(m);
                self.user_set_flags(read_label(false), &[newest], FlagKind::Read, false);
            }
            Shortcut::DeleteForever => self.confirm_purge(vec![m], sender),
            Shortcut::Undo
            | Shortcut::Compose
            | Shortcut::InWindow(WindowCompose::New)
            | Shortcut::Shortcuts
            | Shortcut::Refresh => self.run_shortcut(action, sender),
            _ => {}
        }
    }

    /// Whether the keyboard is in the reader: its message, or anything else
    /// inside the reading pane.
    pub(super) fn reader_has_focus(&self) -> bool {
        let reader = self.message_view.widget().clone().upcast::<gtk::Widget>();
        gtk::prelude::GtkWindowExt::focus(&self.window)
            .is_some_and(|f| f == reader || f.is_ancestor(&reader))
    }

    /// Open the keyboard-shortcut reference, or close it if it is already up:
    /// the key that summons a cheatsheet is the obvious one to dismiss it with.
    fn show_shortcuts(&mut self) {
        if let Some(win) = self.shortcuts_win.take() {
            if win.is_visible() {
                win.close();
                return;
            }
            // Closed from its own titlebar; fall through and open a fresh one.
        }
        self.shortcuts_win = Some(self.build_shortcuts_window());
    }

    /// Whether Gmail's keys are what the keyboard does.
    fn gmail_on(&self) -> bool {
        self.gmail_keys.get() && self.single_key.get()
    }

    /// After Gmail's Shift+U or _ on the open conversation, close it: the
    /// keyboard goes back to its row, which stays unread (#392). Checked
    /// rows stay as they are.
    fn close_after_unread(&self) {
        if self.gmail_on()
            && self.current.is_some()
            && !self.many_selected()
            && self.reader_compose.is_none()
        {
            self.message_list.emit(MessageListInput::CloseOpen);
        }
    }

    /// Gmail's `_` (#392): mark unread from the message being read onward,
    /// that message and the later ones of its conversation. The message is
    /// the picked card, or with none picked the conversation's newest, which
    /// is where Gmail's own cursor sits in an open conversation.
    fn mark_unread_from(&mut self) {
        if self.many_selected() {
            let targets = self.multi_selection();
            self.user_set_flags(read_label(false), &targets, FlagKind::Read, false);
            return;
        }
        if self.current_thread.len() <= 1 {
            if let Some(m) = self.reply_target() {
                self.user_set_flags(read_label(false), &[m], FlagKind::Read, false);
            }
            return;
        }
        let order = |m: &Message| (m.timestamp, m.uid);
        let start = (if self.selection_from_cards {
            self.reply_target()
        } else {
            None
        })
        .or_else(|| self.current_thread.iter().max_by_key(|m| order(m)).cloned());
        let Some(start) = start else { return };
        let from = order(&start);
        let mut targets: Vec<Message> = self
            .conversation_to_mark_unread()
            .into_iter()
            .filter(|m| order(m) >= from && (m.account_id, m.id) != (start.account_id, start.id))
            .collect();
        // The message asked about is marked whatever folder it is in.
        targets.insert(0, start);
        self.hold_unread(&targets);
        self.user_set_flags(read_label(false), &targets, FlagKind::Read, false);
    }

    /// Keep the reader from marking these read again as they sit in view.
    /// The ones being marked unread are told by `set_read`; one that was
    /// unread already is not, and would be read a moment later (#392).
    fn hold_unread(&self, targets: &[Message]) {
        for m in targets.iter().filter(|m| m.unread) {
            if self
                .current_thread
                .iter()
                .any(|t| (t.account_id, t.id) == (m.account_id, m.id))
            {
                self.message_view.emit(MessageViewInput::SuppressAutoRead {
                    account_id: m.account_id,
                    id: m.id,
                });
            }
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_shortcuts(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ShowShortcuts => self.show_shortcuts(),
            AppMsg::CommandPalette(actions) => {
                let entries = self.command_palette_entries(&sender);
                let typed = if actions { ">" } else { "" };
                // Over the window it was asked from: a composer of its own
                // window would otherwise hide it behind.
                let tops = gtk::Window::toplevels();
                let active = (0..tops.n_items())
                    .filter_map(|i| tops.item(i))
                    .filter_map(|o| o.downcast::<gtk::Window>().ok())
                    .find(|w| w.is_active() && w.is_visible());
                match active {
                    Some(w) => crate::ui::command_palette::show(&w, entries, typed),
                    None => crate::ui::command_palette::show(&self.window, entries, typed),
                }
            }

            AppMsg::PrintPreview | AppMsg::PrintMessage => {
                // Only the reader can print: it holds the rendered message, and
                // printing from an empty reader would offer a blank page. Say so
                // rather than appearing to do nothing, which is indistinguishable
                // from a broken menu item.
                let preview = matches!(msg, AppMsg::PrintPreview);
                tracing::info!(preview, open = self.current.is_some(), "print requested");
                if self.current.is_none() {
                    self.notice(i18n("Open a message first, then print it."));
                    return;
                }
                self.message_view.emit(if preview {
                    MessageViewInput::PrintPreview
                } else {
                    MessageViewInput::Print
                });
            }

            AppMsg::Shortcut(action) => self.run_shortcut(action, &sender),
            AppMsg::ZoomKey(step) => {
                // The keys are the application's, so they reach the main
                // window before the composer in it could take them.
                let compose = self
                    .reader_compose
                    .as_ref()
                    .filter(|r| r.window.is_none() && focus_in_compose(&self.window));
                match compose {
                    Some(r) => r.controller.emit(ComposeInput::Zoom(step)),
                    None => sender.input(AppMsg::ZoomMessage(step)),
                }
            }
            AppMsg::ComposerKey(keyval) => {
                let compose = self
                    .reader_compose
                    .as_ref()
                    .filter(|r| r.window.is_none() && focus_in_compose(&self.window));
                let shift_ctrl =
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
                match compose.zip(crate::ui::compose::composer_key(keyval, shift_ctrl)) {
                    Some((r, input)) => r.controller.emit(input),
                    // Activated without a value, the stateful action toggles.
                    None if keyval == gtk::gdk::Key::a => {
                        relm4::main_application().activate_action("show-accounts", None)
                    }
                    None => sender.input(AppMsg::OpenConsole),
                }
            }
            AppMsg::ZoomMessage(step) => {
                use config::READER_ZOOM_STEPS as STEPS;
                let at = STEPS.iter().position(|&z| z == self.zoom).unwrap_or(5) as i32;
                let zoom = match step {
                    0 => self.zoom_default,
                    s => {
                        STEPS
                            [(at + i32::from(s.signum())).clamp(0, STEPS.len() as i32 - 1) as usize]
                    }
                };
                self.set_zoom(zoom);
            }

            AppMsg::CopyReaderSelection => {
                self.message_view.emit(MessageViewInput::CopySelection);
            }
            _ => unreachable!("not routed to on_shortcuts"),
        }
    }

    /// Single-key shortcuts. The controller is on the window in the bubble
    /// phase, so the focused widget always gets first refusal: a search entry
    /// or the composer consumes the letter itself and nothing here fires.
    /// `focus_takes_keys` covers the rest — chiefly the reader's web view,
    /// which handles keys without consuming them.
    pub(super) fn install_key_controller(
        &self,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let keys = gtk::EventControllerKey::new();
        let s = sender.clone();
        let window = root.clone();
        let enabled = self.single_key.clone();
        let gmail = self.gmail_keys.clone();
        let probe = self.message_list.model().selection_probe();
        // A tag key carries the rows selected as it is pressed (#416).
        let shortcut = move |action: Shortcut| match action {
            Shortcut::Tag(n) => AppMsg::TagKey {
                n,
                slots: probe.slots(),
            },
            action => AppMsg::Shortcut(action),
        };
        // The first key of a Gmail sequence (g or *) and when it came.
        let pending: std::rc::Rc<std::cell::Cell<Option<(char, std::time::Instant)>>> =
            Default::default();
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let keyval = without_caps_lock(keyval, state);
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let alt = state.contains(gtk::gdk::ModifierType::ALT_MASK);
            if let Some(done) = modifier_key(&s, &window, keyval, ctrl, shift, alt) {
                return done;
            }
            if let Some(done) = always_on_key(&s, &window, keyval, shift, &shortcut) {
                return done;
            }
            if !enabled.get() || focus_is_text(&window) || focus_in_compose(&window) {
                return gtk::glib::Propagation::Proceed;
            }
            if focus_takes_keys(&window) && reader_keeps(keyval) {
                return gtk::glib::Propagation::Proceed;
            }
            if gmail.get() {
                return gmail_key(&s, keyval, shift, &pending, &shortcut);
            }
            send_shortcut(&s, shortcut_for(keyval, shift), &shortcut)
        });
        root.add_controller(keys);
    }
}

/// The keys held with Ctrl or Alt, which work whether or not single-key
/// shortcuts are on. `None` when the key is not one of them and goes on to
/// the next checks.
fn modifier_key(
    s: &ComponentSender<AppModel>,
    window: &adw::ApplicationWindow,
    keyval: gtk::gdk::Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Option<gtk::glib::Propagation> {
    // Ctrl+? opens the reference whether or not the shortcuts
    // themselves are switched on.
    if ctrl && matches!(keyval, gtk::gdk::Key::question | gtk::gdk::Key::slash) {
        s.input(AppMsg::ShowShortcuts);
        return Some(gtk::glib::Propagation::Stop);
    }
    // Ctrl+Z undoes the last action, Ctrl+Shift+Z and Ctrl+Y put
    // it back (#200). Text fields and the composer keep all three
    // for their own text history.
    let undo_key = ctrl && !shift && keyval == gtk::gdk::Key::z;
    let redo_key = ctrl
        && ((shift && matches!(keyval, gtk::gdk::Key::z | gtk::gdk::Key::Z))
            || keyval == gtk::gdk::Key::y);
    if undo_key || redo_key {
        if focus_is_text(window) || focus_in_compose(window) {
            return Some(gtk::glib::Propagation::Proceed);
        }
        s.input(if redo_key { AppMsg::Redo } else { AppMsg::Undo });
        return Some(gtk::glib::Propagation::Stop);
    }
    // Ctrl+C with the keyboard on the list or the window itself:
    // a selection made in the reader still wants copying (the
    // list takes focus back after a click over a message body).
    // The reader's own document handles the key when it has focus;
    // a text field or composer keeps its native copy.
    if ctrl
        && !shift
        && keyval == gtk::gdk::Key::c
        && !focus_takes_keys(window)
        && !focus_in_compose(window)
    {
        s.input(AppMsg::CopyReaderSelection);
        return Some(gtk::glib::Propagation::Proceed);
    }
    // The keys other mail apps use (#328), with or without
    // single-key shortcuts. A composer keeps them: Ctrl+U
    // underlines there, and a reply should not start from the
    // middle of another.
    if ctrl && !alt && !focus_in_compose(window) {
        if let Some(msg) = ctrl_shortcut_for(keyval, shift) {
            s.input(msg);
            return Some(gtk::glib::Propagation::Stop);
        }
    }
    if ctrl || alt {
        return Some(gtk::glib::Propagation::Proceed);
    }
    None
}

/// Escape and Delete, which work without single-key shortcuts too. `None`
/// when the key is neither, or a text field or composer keeps it.
fn always_on_key(
    s: &ComponentSender<AppModel>,
    window: &adw::ApplicationWindow,
    keyval: gtk::gdk::Key,
    shift: bool,
    shortcut: &impl Fn(Shortcut) -> AppMsg,
) -> Option<gtk::glib::Propagation> {
    // Escape backs out to the message list whatever else is going on,
    // and without needing single-key shortcuts switched on. A text
    // field keeps it, though — there it clears the search.
    if keyval == gtk::gdk::Key::Escape {
        if focus_is_text(window) {
            return Some(gtk::glib::Propagation::Proceed);
        }
        s.input(AppMsg::Shortcut(Shortcut::BackToList));
        return Some(gtk::glib::Propagation::Stop);
    }
    // Delete moves the selection to Trash and Shift+Delete erases
    // it, Thunderbird's key (#471), also without single-key
    // shortcuts. The list catches both first when it has focus;
    // this is the reader and the rest (#486).
    if matches!(keyval, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete)
        && !focus_is_text(window)
        && !focus_in_compose(window)
    {
        s.input(shortcut(if shift {
            Shortcut::DeleteForever
        } else {
            Shortcut::Delete
        }));
        return Some(gtk::glib::Propagation::Stop);
    }
    None
}

/// A single key under Gmail's keys: the second key of a `g` or `*`
/// sequence begun within one and a half seconds, the start of one, or a
/// key of its own.
fn gmail_key(
    s: &ComponentSender<AppModel>,
    keyval: gtk::gdk::Key,
    shift: bool,
    pending: &std::cell::Cell<Option<(char, std::time::Instant)>>,
    shortcut: &impl Fn(Shortcut) -> AppMsg,
) -> gtk::glib::Propagation {
    // Shift alone starts nothing and ends nothing.
    if matches!(keyval, gtk::gdk::Key::Shift_L | gtk::gdk::Key::Shift_R) {
        return gtk::glib::Propagation::Proceed;
    }
    let first = pending
        .take()
        .filter(|(_, at)| at.elapsed() < std::time::Duration::from_millis(1500));
    if let Some((first, _)) = first {
        if let Some(action) = gmail_sequence(first, keyval) {
            s.input(shortcut(action));
        }
        return gtk::glib::Propagation::Stop;
    }
    let start = match keyval {
        gtk::gdk::Key::g => Some('g'),
        gtk::gdk::Key::asterisk => Some('*'),
        _ => None,
    };
    if let Some(c) = start {
        pending.set(Some((c, std::time::Instant::now())));
        return gtk::glib::Propagation::Stop;
    }
    send_shortcut(s, gmail_shortcut_for(keyval, shift), shortcut)
}

/// Run `action` when the key has one; the key goes on to the focused
/// widget when it has none.
fn send_shortcut(
    s: &ComponentSender<AppModel>,
    action: Option<Shortcut>,
    shortcut: &impl Fn(Shortcut) -> AppMsg,
) -> gtk::glib::Propagation {
    match action {
        Some(action) => {
            s.input(shortcut(action));
            gtk::glib::Propagation::Stop
        }
        None => gtk::glib::Propagation::Proceed,
    }
}
