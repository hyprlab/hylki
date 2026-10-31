//! Printing, translation, unsubscribing, invitations and the sender's
//! OpenPGP key: the reader's requests that leave the page.

use super::print::sanitize_filename;
use super::sender::{confirm_trust, fetch_sender_key};
use super::types::{InviteAction, MessageViewInput, MessageViewOutput, TrState, UnsubState};
use super::MessageView;
use crate::i18n::i18n;
use crate::models::Message;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageView {
    pub(super) fn on_print(&mut self) {
        crate::ui::print_preview::print_html(
            &self.print_document_html(),
            &sanitize_filename(&self.job_name()),
            self.webview.root().and_downcast::<gtk::Window>(),
        );
    }

    pub(super) fn on_print_preview(&mut self) {
        // Shown inside Hylki rather than exported to a PDF and handed to
        // whatever the desktop opens PDFs with: that route is a temporary
        // file, a URI, the document portal and an external viewer, each
        // able to fail without saying anything — and it did.
        // A pop-out reader's window is not an application window,
        // and asking for one there showed nothing (#359).
        let Some(parent) = self.webview.root().and_downcast::<gtk::Window>() else {
            tracing::warn!("no window to attach the preview to");
            return;
        };
        crate::ui::print_preview::open(
            &parent,
            &self.print_document_html(),
            &sanitize_filename(&self.job_name()),
        );
    }

    /// Print messages the reader is not showing, with its fonts, Reader
    /// View and remote-content choice: the page is built at once, and the
    /// reader is left as it was.
    pub(super) fn on_print_messages(
        &mut self,
        message: Message,
        thread: Vec<Message>,
        allow_remote: bool,
        preview: bool,
    ) {
        let thread = std::mem::replace(&mut self.thread, thread);
        let current = self.current.replace(message);
        let remote = std::mem::replace(&mut self.remote_allowed, allow_remote);
        if preview {
            self.on_print_preview();
        } else {
            self.on_print();
        }
        self.thread = thread;
        self.current = current;
        self.remote_allowed = remote;
    }

    pub(super) fn on_toggle_sender_style(&mut self, account_id: u32, id: u32) {
        let key = (account_id, id);
        if !self.sender_style.remove(&key) {
            self.sender_style.insert(key);
        }
        if self.current.is_some() && !self.loading {
            self.render();
        }
    }

    pub(super) fn on_sender_check_for(
        &mut self,
        account_id: u32,
        id: u32,
        check: Box<crate::models::SenderCheck>,
    ) {
        self.member_checks.insert((account_id, id), *check);
        if self.webview_ready {
            self.patch_verify_badge(account_id, id);
            self.patch_unsub(account_id, id);
            self.patch_invite(account_id, id);
            self.patch_receipt(account_id, id);
        }
    }

    pub(super) fn on_translate(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        let key = (account_id, id);
        match self.translations.get(&key).cloned() {
            Some(TrState::Working) => {}
            Some(TrState::Shown(t)) => {
                self.translations.insert(key, TrState::Original(t));
                self.render();
            }
            Some(TrState::Original(t)) => {
                self.translations.insert(key, TrState::Shown(t));
                self.render();
            }
            None | Some(TrState::Failed(_)) => {
                let Some(m) = self
                    .thread
                    .iter()
                    .find(|m| (m.account_id, m.id) == key)
                    .cloned()
                else {
                    return;
                };
                let settings = crate::services::translate::load();
                let refused = if self.encrypted(key) {
                    Some(i18n("Encrypted messages are not sent for translation."))
                } else if !settings.enabled() {
                    Some(i18n(
                        "Choose a translation service in Settings → Translation first.",
                    ))
                } else if m.body.trim().is_empty() {
                    Some(i18n("The message has not loaded yet."))
                } else {
                    None
                };
                if let Some(why) = refused {
                    self.translations.insert(key, TrState::Failed(why));
                    self.patch_translate(account_id, id);
                    return;
                }
                let cache = crate::services::translate::cache_key(
                    account_id,
                    if m.message_id.is_empty() {
                        &m.body
                    } else {
                        &m.message_id
                    },
                    &settings,
                );
                // Already translated this session: no second request.
                if let Some(t) = crate::services::translate::cached(&cache) {
                    self.translations.insert(key, TrState::Shown(t));
                    self.render();
                    return;
                }
                self.translations.insert(key, TrState::Working);
                self.patch_translate(account_id, id);
                let input = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let api_key =
                        crate::services::translate::load_key(settings.service).unwrap_or_default();
                    let result = crate::services::translate::translate(
                        &settings, &api_key, &m.body, &m.subject, &cache,
                    );
                    input.emit(MessageViewInput::Translated {
                        account_id,
                        id,
                        result,
                    });
                });
            }
        }
    }

    pub(super) fn on_translated(
        &mut self,
        account_id: u32,
        id: u32,
        result: Result<crate::services::translate::Translated, String>,
    ) {
        let key = (account_id, id);
        // The reader moved on to another message meanwhile.
        if self.translations.get(&key) != Some(&TrState::Working) {
            return;
        }
        match result {
            Ok(t) => {
                self.translations.insert(key, TrState::Shown(t));
                self.render();
            }
            Err(why) => {
                tracing::warn!("translation failed: {why}");
                self.translations.insert(key, TrState::Failed(why));
                self.patch_translate(account_id, id);
            }
        }
    }

    pub(super) fn on_unsubscribe(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        let info = self
            .member_checks
            .get(&(account_id, id))
            .and_then(|c| c.unsubscribe.clone());
        let message = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
            .cloned();
        if let (Some(info), Some(message)) = (info, message) {
            let _ = sender.output(MessageViewOutput::Unsubscribe {
                message: Box::new(message),
                info: Box::new(info),
            });
        }
    }

    pub(super) fn on_receipt_answer(
        &mut self,
        account_id: u32,
        id: u32,
        send: bool,
        sender: &ComponentSender<Self>,
    ) {
        let message = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
            .cloned();
        if let Some(message) = message {
            // The banner goes at once; the app's answer would take it away
            // a moment later anyway.
            if let Some(c) = self.member_checks.get_mut(&(account_id, id)) {
                c.receipt = None;
            }
            self.patch_receipt(account_id, id);
            let _ = sender.output(MessageViewOutput::ReceiptAnswer {
                message: Box::new(message),
                send,
            });
        }
    }

    pub(super) fn on_unsubscribe_state(
        &mut self,
        account_id: u32,
        id: u32,
        state: Option<UnsubState>,
    ) {
        match state {
            Some(s) => {
                self.unsub_state.insert((account_id, id), s);
            }
            None => {
                self.unsub_state.remove(&(account_id, id));
            }
        }
        self.patch_unsub(account_id, id);
    }

    pub(super) fn on_invite_action(
        &mut self,
        account_id: u32,
        id: u32,
        action: InviteAction,
        sender: &ComponentSender<Self>,
    ) {
        let invite = self
            .member_checks
            .get(&(account_id, id))
            .and_then(|c| c.invite.clone());
        let message = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
            .cloned();
        if let (Some(invite), Some(message)) = (invite, message) {
            let _ = sender.output(MessageViewOutput::InviteAction {
                message: Box::new(message),
                invite,
                action,
            });
        }
    }

    pub(super) fn on_sender_info_at(
        &mut self,
        account_id: u32,
        id: u32,
        rect: (f64, f64, f64, f64),
        page_width: f64,
        sender: &ComponentSender<Self>,
    ) {
        self.show_sender_popover(account_id, id, rect, page_width, sender);
    }

    pub(super) fn on_pgp_fetch_key(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        let Some(pgp) = self
            .member_checks
            .get(&(account_id, id))
            .and_then(|c| c.pgp.clone())
        else {
            return;
        };
        let input = sender.input_sender().clone();
        sender.oneshot_command(async move {
            let result = tokio::task::spawn_blocking(move || fetch_sender_key(&pgp))
                .await
                .unwrap_or_else(|_| Err("task failed".into()));
            let _ = input.send(MessageViewInput::PgpDone {
                account_id,
                id,
                result,
            });
        });
    }

    pub(super) fn on_pgp_trust_key(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        let Some(pgp) = self
            .member_checks
            .get(&(account_id, id))
            .and_then(|c| c.pgp.clone())
        else {
            return;
        };
        let crate::models::PgpSignature::Good { signer, key_id, .. } = &pgp.signature else {
            return;
        };
        // gpg on a worker thread: a stale keyring lock makes it
        // wait about ten seconds (#316).
        let signer = signer.clone();
        let key_id = key_id.clone();
        let parent = self.webview.root().and_downcast::<gtk::Window>();
        let s = sender.clone();
        gtk::glib::spawn_future_local(async move {
            let key = gtk::gio::spawn_blocking(move || {
                crate::mail::pgp::key_by_fingerprint(&crate::mail::pgp::Gpg::system(), &key_id)
            })
            .await
            .ok()
            .flatten();
            let Some(key) = key else {
                let _ = s.output(MessageViewOutput::Notice(i18n(
                    "That key is no longer in your keyring.",
                )));
                return;
            };
            confirm_trust(parent.as_ref(), &signer, &key, s, account_id, id);
        });
    }

    pub(super) fn on_pgp_trust_confirmed(
        &mut self,
        account_id: u32,
        id: u32,
        fingerprint: String,
        sender: &ComponentSender<Self>,
    ) {
        let input = sender.input_sender().clone();
        sender.oneshot_command(async move {
            let result = tokio::task::spawn_blocking(move || crate::mail::pgp::trust_key(&crate::mail::pgp::Gpg::system(), &fingerprint, None))
                .await
                .unwrap_or_else(|_| Err("task failed".into()))
                .map(|()| i18n("Key trusted."))
                .map_err(|e| {
                    if e.contains("secret key") || e.contains("default") {
                        i18n("Generate a key of your own first (Settings, OpenPGP): trusting a key means signing it with yours.")
                    } else {
                        e
                    }
                });
            let _ = input.send(MessageViewInput::PgpDone { account_id, id, result });
        });
    }

    pub(super) fn on_pgp_done(
        &mut self,
        account_id: u32,
        id: u32,
        result: Result<String, String>,
        sender: &ComponentSender<Self>,
    ) {
        match result {
            Ok(text) => {
                let _ = sender.output(MessageViewOutput::Notice(text));
                if let Some(m) = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                {
                    let _ = sender.output(MessageViewOutput::ReloadBody(Box::new(m.clone())));
                }
            }
            Err(e) => {
                let _ = sender.output(MessageViewOutput::Notice(e));
            }
        };
    }
}
