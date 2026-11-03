//! Showing a message: `Show`, the remote-content choices, and the theme and
//! paint events that render it again.

use super::remote::has_remote_resources;
use super::types::{MessageViewInput, MessageViewOutput};
use super::MessageView;
use crate::models::Message;
use adw::prelude::*;
use relm4::prelude::*;
use webkit6::prelude::WebViewExt;

/// How long a message still being fetched leaves the last one on screen
/// before it shows, with its spinner: a wait shorter than this looks like
/// none, and a spinner flashed for a moment looks like a glitch.
const LOADING_AFTER: std::time::Duration = std::time::Duration::from_millis(300);

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
        sender: &ComponentSender<Self>,
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
        if self.current.is_none() {
            // The empty pane: nothing to wait for.
            self.swap.reveal(self.swap.generation(), "empty pane");
        } else if self.loading {
            // Still being fetched: the last message stays, frozen, for a
            // moment, and then this one shows with its spinner.
            let s = sender.input_sender().clone();
            let generation = self.swap.generation();
            gtk::glib::timeout_add_local_once(LOADING_AFTER, move || {
                s.emit(MessageViewInput::LoadingDeadline(generation))
            });
        } else {
            let loads = self.render();
            // Stepping to another message of the same conversation
            // repaints nothing, but that message must open (#326).
            self.patch_folds();
            // Nothing new to load: what is there, opened, is the message.
            if !loads {
                self.reveal_when_drawn();
            }
        }
    }

    /// See [`MessageViewInput::LoadingDeadline`].
    pub(super) fn on_loading_deadline(&mut self, generation: u64) {
        if self.loading {
            self.swap.reveal(generation, "still loading: spinner");
        }
    }

    /// Show the pane as it now is, once the page has taken what was patched
    /// into it on arrival (queued ahead of this, so it goes in first:
    /// attachment chips, verdicts) and its scroll for them, and has been
    /// drawn. A row patched in after the page is on screen pushes it down,
    /// and the page then scrolls again to keep its card in place. A page
    /// that loaded behind the spinner was never laid out: its frames are
    /// measured again now that it shows.
    pub(super) fn reveal_when_drawn(&self) {
        // The newest page is still loading: its own readiness reveals it.
        if self.ready_uri != self.latest_uri {
            return;
        }
        let swap = self.swap.clone();
        let generation = swap.generation();
        let latest = self.latest_uri.clone();
        let view = self.webview.clone();
        self.webview.call_async_javascript_function(
            "try{var fs=all();for(var i=0;i<fs.length;i++)s(fs[i]);chase();pin();}catch(_){}\
             await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));",
            None,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |_| {
                // A page that has since been replaced: the newest one,
                // once ready, ends the freeze.
                if view.uri().unwrap_or_default() == latest {
                    swap.reveal(generation, "page drawn");
                }
            },
        );
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
        tracing::debug!(
            target: "hylki::swap",
            "page ready: {} (newest {})",
            self.webview.uri().unwrap_or_default(),
            self.latest_uri
        );
        self.webview_ready = true;
        self.ready_uri = self.webview.uri().unwrap_or_default().to_string();
        // Verdicts that arrived while the document was still loading.
        for (aid, id) in self.member_checks.keys().copied().collect::<Vec<_>>() {
            self.patch_verify_badge(aid, id);
            self.patch_unsub(aid, id);
            self.patch_invite(aid, id);
            self.patch_receipt(aid, id);
        }
        // Likewise attachments (#213): the document was built from
        // what was known when the render was queued.
        let keys: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        self.patch_card_atts(&keys);
        self.patch_owed_faces();
        // After the patches: the page is shown with them in.
        self.reveal_when_drawn();
    }
}
