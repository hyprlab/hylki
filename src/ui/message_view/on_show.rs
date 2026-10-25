//! Showing a message: `Show`, the remote-content choices, and the theme and
//! paint events that render it again.

use super::remote::has_remote_resources;
use super::types::MessageViewOutput;
use super::MessageView;
use crate::models::Message;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageView {
    // The fields of `MessageViewInput::Show`, one to one.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn on_show(
        &mut self,
        thread: Vec<Message>,
        allow_remote: bool,
        account_name: Option<String>,
        account_color: Option<String>,
        loading: bool,
        primary: Option<Box<Message>>,
        folder_labels: std::collections::HashMap<(u32, u32), String>,
        instant: bool,
        sent: std::collections::HashSet<(u32, u32)>,
    ) {
        let shown = primary.map(|p| *p).or_else(|| thread.first().cloned());
        // A new message: the previous message's verdict must not linger
        // on screen while this one's is still being fetched.
        let same_message = self
            .current
            .as_ref()
            .zip(shown.as_ref())
            .is_some_and(|(a, b)| a.id == b.id && a.account_id == b.account_id);
        if !same_message {
            self.sender_check = None;
            self.member_checks.clear();
            self.unsub_state.clear();
        }
        // A card taken out of the conversation on screen, deleted or
        // moved, can leave another message heading it. What remains
        // is the same document, read on from where it was.
        let card_left = !same_message
            && thread.len() < self.thread.len()
            && thread.iter().all(|m| {
                self.thread
                    .iter()
                    .any(|t| (t.account_id, t.id) == (m.account_id, m.id))
            });
        // What was done to a card (translated, folded) holds while it
        // stays in the conversation on screen: stepping to another of
        // its messages must not undo it. The message stepped to opens.
        let stays = |k: &(u32, u32)| thread.iter().any(|m| (m.account_id, m.id) == *k);
        self.translations.retain(|k, _| stays(k));
        self.folds.retain(|k, _| stays(k));
        if !same_message && !card_left {
            if let Some(key) = shown.as_ref().map(|s| (s.account_id, s.id)) {
                if self.thread.iter().any(|m| (m.account_id, m.id) == key) {
                    self.folds.insert(key, false);
                }
            }
        }
        self.link_preview.set_visible(false);
        self.current = shown;
        // A fresh open: anything the user marked unread earlier can be
        // read by scrolling again — and the first render may auto-scroll
        // to the unread mark again.
        if !same_message && !card_left {
            self.no_autoread.clear();
            self.did_autoscroll = false;
            self.saved_anchor = None;
            self.scroll_owed = false;
            // A per-message Reader View default (Settings) resets
            // the view for each conversation opened; the switch can
            // still flip this one.
            if let Some(on) = self.reader_default.starts_on() {
                self.set_reader_mode(on);
            }
        }
        self.thread = thread;
        self.folder_labels = folder_labels;
        self.sent = sent;
        self.account_name = account_name;
        self.loading = loading;
        self.instant = instant;
        if account_color.is_some() {
            self.account_color = account_color;
            self.paint_account_chip();
        }
        self.remote_allowed = allow_remote;
        let has_remote = self.thread.iter().any(|m| has_remote_resources(&m.body));
        self.blocked = has_remote && !allow_remote;
        // Repaint the cover for what is arriving, even when the spinner
        // is about to be shown instead of a document: the whole point is
        // that the spinner already sits on the right color.
        self.apply_webview_bg(self.effective_dark());
        // While loading, the spinner page is shown; rendering the (empty)
        // body would just flash blank, so wait for the real body.
        if !self.loading {
            self.render();
            // Stepping to another message of the same conversation
            // repaints nothing, but that message must open (#326).
            self.patch_folds();
        }
    }

    pub(super) fn on_load_remote_once(&mut self, sender: &ComponentSender<Self>) {
        // Tell the app as well, so the choice survives the next repaint
        // and the reader menu stops offering to do what is already done.
        if let Some(m) = &self.current {
            let _ = sender.output(MessageViewOutput::SetRemote {
                account_id: m.account_id,
                id: m.id,
                show: true,
            });
        }
        self.remote_allowed = true;
        self.blocked = false;
        self.render();
    }

    pub(super) fn on_toggle_remote(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        let show = !self.remote_allowed;
        self.remote_allowed = show;
        self.blocked = !show && self.thread.iter().any(|m| has_remote_resources(&m.body));
        let _ = sender.output(MessageViewOutput::SetRemote {
            account_id,
            id,
            show,
        });
        // The app records it and re-shows the message; that render is
        // free (the fingerprint already matches), so paint now.
        self.render();
    }

    pub(super) fn on_allow_sender_always(&mut self, sender: &ComponentSender<Self>) {
        if let Some(m) = &self.current {
            let _ = sender.output(MessageViewOutput::AllowSender(m.from_addr.clone()));
        }
        self.remote_allowed = true;
        self.blocked = false;
        self.render();
    }

    pub(super) fn on_theme_changed(&mut self) {
        self.paint_account_chip();
        let dark = self.effective_dark();
        self.apply_webview_bg(dark);
        if self.current.is_some() && !self.loading {
            self.render();
        }
    }

    pub(super) fn on_mapped(&mut self) {
        if std::mem::take(&mut self.painted_hidden) && self.current.is_some() && !self.loading {
            self.shown_fingerprint = None;
            self.render();
        }
    }

    pub(super) fn on_rendered(&mut self) {
        self.webview_ready = true;
        // Verdicts that arrived while the document was still loading.
        for (aid, id) in self.member_checks.keys().copied().collect::<Vec<_>>() {
            self.patch_verify_badge(aid, id);
            self.patch_unsub(aid, id);
            self.patch_invite(aid, id);
        }
        // Likewise attachments (#213): the document was built from
        // what was known when the render was queued.
        let keys: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        self.patch_card_atts(&keys);
    }
}
