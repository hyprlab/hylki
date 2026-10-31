//! The reading pane switched off (#475): the list takes the whole window,
//! a selected message is only selected, and mail is read in its own window.

use std::cell::Cell;

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::models::Message;
use crate::ui::attachment_drawer::AttachmentDrawerInput;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;

use super::conversation::body_key;
use super::model::AppModel;
use super::msg::AppMsg;
use super::reader_toolbar::{detach, ReaderToolbarWidgets};

/// Where the toolbar folds in the list header (#475). The reader's own
/// breakpoints measure the reader pane, which is hidden, so the list's
/// width decides instead: the strip over the list reports it.
#[derive(Default)]
pub(crate) struct ListFold {
    active: Cell<bool>,
    width: Cell<i32>,
    /// Below this the right group goes into the ⋯ menu.
    full: Cell<i32>,
    /// Below this the buttons draw narrower as well.
    compact: Cell<i32>,
    /// What the app was last told: (collapsed, compact).
    told: Cell<Option<(bool, bool)>>,
}

impl ListFold {
    pub(super) fn resized(&self, width: i32, sender: &relm4::Sender<AppMsg>) {
        self.width.set(width);
        self.check(sender);
    }

    pub(super) fn set_needs(&self, full: i32, compact: i32, sender: &relm4::Sender<AppMsg>) {
        self.full.set(full);
        self.compact.set(compact);
        self.check(sender);
    }

    pub(super) fn set_active(&self, on: bool, sender: &relm4::Sender<AppMsg>) {
        self.active.set(on);
        self.told.set(None);
        self.check(sender);
    }

    /// As the reader's breakpoints do: the compact one applies alone.
    fn check(&self, sender: &relm4::Sender<AppMsg>) {
        let width = self.width.get();
        if !self.active.get() || width <= 0 {
            return;
        }
        let compact = width <= self.compact.get();
        let collapsed = !compact && width <= self.full.get();
        if self.told.replace(Some((collapsed, compact))) != Some((collapsed, compact)) {
            let _ = sender.send(AppMsg::SetListFold { collapsed, compact });
        }
    }
}

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_reading_pane(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::SetReadingPane(pane) => self.set_reading_pane(pane),
            AppMsg::SetListFold { collapsed, compact } => {
                self.list_actions_collapsed = collapsed;
                self.list_compact = compact;
                self.reader_overflow_btn
                    .set_visible(self.reader_overflow_wanted());
                if let Some(tb) = self.reader_toolbar_widgets.get() {
                    if compact {
                        tb.list_tools.add_css_class("compact");
                    } else {
                        tb.list_tools.remove_css_class("compact");
                    }
                }
                let s = self.self_sender.clone();
                self.after_paint(move || {
                    let _ = s.send(AppMsg::MeasureReaderFold);
                });
            }
            AppMsg::PopoutSeen { account_id, id } => self.mark_window_card_read(account_id, id),
            _ => unreachable!("not routed to on_reading_pane"),
        }
    }

    fn set_reading_pane(&mut self, pane: config::ReadingPane) {
        let key = pane.key();
        if self
            .reading_pane_action
            .state()
            .and_then(|v| v.get::<String>())
            .as_deref()
            != Some(key)
        {
            self.reading_pane_action.set_state(&key.to_variant());
        }
        let was = self.reading_pane;
        if !pref!(self.reading_pane = pane) {
            return;
        }
        // Only the reader beside the list writes in it: an inline reply is
        // saved or kept in its own window.
        if was == config::ReadingPane::Right {
            self.release_reader_compose();
        }
        if was == config::ReadingPane::Off {
            // The reader shows what is selected now. It was never drawn
            // while the pane was off, so it must not look already shown.
            let open = self.current.as_ref().map(|c| (c.account_id, c.id));
            self.blank_reader();
            if let Some(key) = open {
                self.message_list.emit(MessageListInput::SelectAndLoad(key));
            }
        } else if !self.reader_shown() {
            // The hidden reader lets go of what it was showing. The
            // selection stays, for the keys and menus that act on it.
            self.attachments.clear();
            self.attachments_loading = false;
            self.sync_attachment_drawer();
            self.show_message(None, false);
            self.message_list.emit(MessageListInput::ReclaimFocus);
        }
        self.place_reader();
        self.push_single_line();
        self.list_fold
            .set_active(self.toolbar_in_list(), &self.self_sender);
        self.relayout_reader_toolbar();
        self.reader_overflow_btn
            .set_visible(self.reader_overflow_wanted());
    }

    /// Whether the reader is there at all.
    pub(super) fn reader_shown(&self) -> bool {
        self.reading_pane != config::ReadingPane::Off
    }

    /// Whether the toolbar is in the list's header, at the top of the
    /// window: with the reader off. Below the list, it stays on the reader.
    pub(super) fn toolbar_in_list(&self) -> bool {
        self.reading_pane == config::ReadingPane::Off
    }

    /// Whether the window's controls are in the list's header: with the
    /// reader below the list or off, the list's header is the window's top.
    pub(super) fn controls_in_list(&self) -> bool {
        self.reading_pane != config::ReadingPane::Right
    }

    /// Turn the split for where the reader sits, at the list's remembered
    /// width or height. Below, the attachment drawer starts folded: the
    /// pane is short.
    pub(super) fn place_reader(&self) {
        let Some(split) = self.mail_split.get() else {
            return;
        };
        let below = self.reading_pane == config::ReadingPane::Below;
        let (orientation, position) = if below {
            (gtk::Orientation::Vertical, config::load_list_pane_height())
        } else {
            (gtk::Orientation::Horizontal, config::load_list_pane_width())
        };
        if split.orientation() != orientation {
            split.set_orientation(orientation);
            split.set_position(position);
        }
        if below {
            self.attachment_drawer.emit(AttachmentDrawerInput::Fold);
        }
    }

    /// Whether the toolbar's right group is folded for want of room, and
    /// whether its buttons draw narrower, in whichever header holds it.
    pub(super) fn toolbar_fold(&self) -> (bool, bool) {
        if !self.toolbar_in_list() {
            (self.reader_actions_collapsed, self.reader_compact)
        } else {
            (self.list_actions_collapsed, self.list_compact)
        }
    }

    /// The toolbar in the list header, in the saved order: the Outbox's
    /// buttons, the left group, the right group and the ⋯, evenly spaced.
    pub(super) fn layout_list_tools(&self, tb: &ReaderToolbarWidgets) {
        let overflow: gtk::Widget = self.reader_overflow_btn.clone().upcast();
        let outbox = tb.outbox.iter().map(|r| r.clone().upcast::<gtk::Widget>());
        let all: Vec<gtk::Widget> = outbox
            .chain(tb.buttons.iter().map(|(_, w)| w.clone()))
            .chain([tb.spinner.clone(), overflow.clone()])
            .collect();
        for w in &all {
            detach(w);
        }
        let find =
            |item: &config::ToolbarItem| tb.buttons.iter().find(|(i, _)| i == item).map(|(_, w)| w);
        for r in &tb.outbox {
            tb.list_tools.append(r);
        }
        for w in self.reader_toolbar.left.iter().filter_map(find) {
            tb.list_tools.append(w);
        }
        tb.list_tools.append(&tb.spinner);
        for w in self.reader_toolbar.right.iter().filter_map(find) {
            tb.list_tools.append(w);
        }
        tb.list_tools.append(&overflow);
    }

    /// A row selected with the pane off: it becomes the message the keys,
    /// menus and actions mean, without being shown, fetched or marked read.
    pub(super) fn select_without_reader(
        &mut self,
        m: Message,
        thread: Vec<Message>,
        sender: &ComponentSender<Self>,
    ) {
        self.release_reader_compose();
        self.attachments.clear();
        self.attachments_loading = false;
        self.sync_attachment_drawer();
        self.listed_thread = if thread.is_empty() {
            vec![(m.account_id, m.id)]
        } else {
            thread.iter().map(|t| (t.account_id, t.id)).collect()
        };
        // A conversation's row means all of it, to Delete and Archive.
        self.current_thread = if thread.len() > 1 { thread } else { Vec::new() };
        self.thread_key = None;
        self.thread_related_pending = false;
        let mut current = m;
        if current.body.is_empty() {
            if let Some(body) = self.body_cache.get(&body_key(&current)) {
                current.body = body.clone();
            }
        }
        self.current = Some(current);
        // Gmail's o or Enter asked for it to be read: in a window.
        if std::mem::take(&mut self.reader_focus_pending) {
            self.open_selected_window(sender);
        }
    }

    /// The selected message, or its conversation, in its own window.
    pub(super) fn open_selected_window(&mut self, sender: &ComponentSender<Self>) {
        let Some(message) = self.current.clone() else {
            return;
        };
        let thread = if self.current_thread.len() > 1 {
            self.current_thread.clone()
        } else {
            Vec::new()
        };
        sender.input(AppMsg::OpenMessageWindow { message, thread });
    }

    /// Opening a message in a window is reading it, while there is no pane
    /// to read it in: a lone message is marked read as the setting says.
    /// A conversation is marked card by card as the window shows them.
    pub(super) fn mark_window_read(&mut self, m: &Message, conversation: bool) {
        if self.reader_shown() || conversation || !m.unread {
            return;
        }
        if self.read_mark != config::ReadMark::Manual {
            self.mark_opened_read(m);
        }
    }

    /// A conversation card read through in a message window.
    fn mark_window_card_read(&mut self, account_id: u32, id: u32) {
        if self.reader_shown() {
            return;
        }
        let folder = self.popouts.values().find_map(|p| {
            p.members
                .iter()
                .find(|(_, key)| *key == (account_id, id))
                .map(|((_, folder_id, _), _)| *folder_id)
        });
        let Some(folder_id) = folder else {
            return;
        };
        if let Some(m) = self
            .shown_copy_in(account_id, folder_id, id)
            .filter(|m| m.unread)
        {
            self.mark_opened_read(&m);
        }
    }

    /// Print with no reading pane: the selected message, or its whole
    /// conversation, straight to the printer or the preview, once every
    /// body is here. The hidden reader builds the page.
    pub(super) fn print_without_reader(&mut self, preview: bool) {
        let Some(m) = self.current.clone() else {
            return;
        };
        let thread = if self.current_thread.len() > 1 {
            // In the order the reader and a message window show it.
            let mut listed = self.current_thread.clone();
            if self.thread_newest_first {
                listed.reverse();
            }
            self.thread_cache
                .get(&body_key(&m))
                .cloned()
                .unwrap_or(listed)
        } else {
            vec![m.clone()]
        };
        let thread: Vec<Message> = thread
            .into_iter()
            .map(|mut t| {
                if t.body.is_empty() {
                    match self.body_cache.get(&body_key(&t)) {
                        Some(body) => t.body = body.clone(),
                        None => self.request_body(&t),
                    }
                }
                t
            })
            .collect();
        self.pending_print = Some(PendingPrint {
            message: m,
            thread,
            preview,
        });
        self.try_pending_print();
    }

    /// A body arrived: one a print may be waiting for.
    pub(super) fn print_body_arrived(&mut self, account_id: u32, id: u32, body: &str) {
        let Some(p) = &mut self.pending_print else {
            return;
        };
        for t in p.thread.iter_mut() {
            if (t.account_id, t.id) == (account_id, id) {
                t.body = body.to_string();
            }
        }
        self.try_pending_print();
    }

    fn try_pending_print(&mut self) {
        if self
            .pending_print
            .as_ref()
            .is_none_or(|p| p.thread.iter().any(|t| t.body.is_empty()))
        {
            return;
        }
        let Some(p) = self.pending_print.take() else {
            return;
        };
        let allow_remote = self.remote_allowed(&p.message);
        self.message_view.emit(MessageViewInput::PrintMessages {
            message: Box::new(p.message),
            thread: p.thread,
            allow_remote,
            preview: p.preview,
        });
    }

    fn request_body(&self, m: &Message) {
        if let Some(path) = self.resolve_folder_path(m) {
            self.send_to(
                m.account_id,
                crate::worker::MailRequest::LoadBody {
                    message_id: m.id,
                    path,
                    uid: m.uid,
                },
            );
        }
    }
}

/// A print asked for with the pane off, waiting for the bodies it needs.
pub(crate) struct PendingPrint {
    message: Message,
    thread: Vec<Message>,
    preview: bool,
}
