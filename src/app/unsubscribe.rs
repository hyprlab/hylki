//! One-click unsubscribe (RFC 2369/8058): the banner state and acting on it.

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::{FolderKind, Message};
use crate::ui::message_view::{MessageViewInput, UnsubState};
use crate::ui::message_window::MessageWindowInput;

use super::conversation::body_key;
use super::demo::demo_mode;
use super::flags::sender_label;
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::first_recipient;
use super::send::machine_message;
use crate::ui::showcase::showcase_capture;

impl AppModel {
    /// The lists left, as the readers take them: key → when.
    pub(super) fn unsubscribed_map(&self) -> HashMap<String, i64> {
        self.unsubscribed
            .iter()
            .map(|l| (l.key.clone(), l.at))
            .collect()
    }

    /// Tell every reader (the main one and the pop-outs) where a card's
    /// unsubscribe request stands.
    fn push_unsubscribe_state(&self, key: (u32, u32), state: Option<UnsubState>) {
        self.message_view.emit(MessageViewInput::UnsubscribeState {
            account_id: key.0,
            id: key.1,
            state: state.clone(),
        });
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::UnsubscribeState {
                account_id: key.0,
                id: key.1,
                state: state.clone(),
            });
        }
    }

    /// Remember that this message's list was left, and tell the readers.
    fn record_unsubscribed(&mut self, message: &Message) {
        let Some(info) = self
            .sender_cache
            .get(&body_key(message))
            .and_then(|c| c.unsubscribe.as_ref())
        else {
            return;
        };
        let key = info.key(&message.from_addr);
        self.unsubscribed.retain(|l| l.key != key);
        self.unsubscribed.push(config::UnsubscribedList {
            key,
            name: sender_label(message),
            at: crate::datefmt::now(),
        });
        config::save_unsubscribed(&self.unsubscribed);
        let map = self.unsubscribed_map();
        self.message_view
            .emit(MessageViewInput::SetUnsubscribed(map.clone()));
        self.popouts_emit(|| MessageWindowInput::SetUnsubscribed(map.clone()));
    }

    /// The identity an unsubscribe mail leaves under: the alias the message
    /// was addressed to when it was one (a list knows its subscriber by the
    /// address it writes to), else the account itself. Returns the send-as
    /// alias for the wire (`None` for the account) and the bare address.
    pub(super) fn unsubscribe_from(&self, message: &Message) -> (Option<String>, String) {
        let Some(cfg) = self.account_config(self.sending_account(message)) else {
            return (None, String::new());
        };
        let recipients: Vec<String> = [&message.to, &message.cc]
            .into_iter()
            .flat_map(|f| crate::worker::parse_recipients(f))
            .map(|(_, e)| e.to_lowercase())
            .collect();
        for alias in &cfg.aliases {
            if let Some((_, addr)) = first_recipient(&alias.identity) {
                if recipients.iter().any(|r| *r == addr.to_lowercase()) {
                    return (Some(alias.identity.clone()), addr);
                }
            }
        }
        (None, cfg.email.clone())
    }

    /// Where an unsubscribe mail goes: the list's `mailto:` handle when it
    /// gave one, else the "reply with UNSUBSCRIBE" it asked for. `None`
    /// when the message offers neither.
    fn unsubscribe_mail_target(
        info: &crate::models::Unsubscribe,
    ) -> Option<crate::mail::unsubscribe::MailtoTarget> {
        if let Some(t) = info
            .mailto
            .as_deref()
            .and_then(crate::mail::unsubscribe::parse_mailto)
        {
            return Some(t);
        }
        info.reply
            .as_ref()
            .map(|r| crate::mail::unsubscribe::MailtoTarget {
                to: r.to.clone(),
                subject: r.subject.clone(),
                body: r.subject.clone(),
            })
    }

    /// The mail in an account from `addr` that leaving a list can take with
    /// it (#407): every folder but Sent, Drafts, Templates and Trash, where
    /// it is either the user's own or already on its way out.
    pub(super) fn mail_from_address(&self, account_id: u32, addr: &str) -> Vec<Message> {
        let Some(cache) = self.cache.as_ref() else {
            return Vec::new();
        };
        let Some(folders) = self.folders.get(&account_id) else {
            return Vec::new();
        };
        let mut by_path: HashMap<String, std::collections::HashSet<u32>> = HashMap::new();
        for (path, uid) in cache.messages_from_address(account_id, addr) {
            by_path.entry(path).or_default().insert(uid);
        }
        let mut out = Vec::new();
        for f in folders {
            if matches!(
                f.kind,
                FolderKind::Sent | FolderKind::Drafts | FolderKind::Templates | FolderKind::Trash
            ) {
                continue;
            }
            let Some(uids) = by_path.get(&f.path) else {
                continue;
            };
            out.extend(
                cache
                    .load_messages(account_id, &f.path, f.id)
                    .into_iter()
                    .filter(|m| uids.contains(&m.uid)),
            );
        }
        out
    }

    /// Ask before leaving a list: what will happen depends on the route the
    /// list offers, and a mail sent in the user's name is said so up front.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn confirm_unsubscribe(
        &self,
        message: Message,
        info: crate::models::Unsubscribe,
        sender: &ComponentSender<Self>,
    ) {
        let heading = i18n_f(
            "Unsubscribe from {name}?",
            &[("name", &sender_label(&message))],
        );
        let mut body = if info.one_click.is_some() {
            i18n("Hylki will ask the list to stop sending you mail. A list can take a few days to act on the request.")
        } else if let Some(t) = Self::unsubscribe_mail_target(&info) {
            let (_, from) = self.unsubscribe_from(&message);
            i18n_f(
                "An unsubscribe request will be sent to {addr} from {from}. A list can take a few days to act on it.",
                &[("addr", &t.to), ("from", &from)],
            )
        } else {
            i18n("This list offers no direct way to unsubscribe. Its unsubscribe page will open in your browser.")
        };
        // Read out of the message's words rather than its headers: a good
        // guess, but the user should know it is one before anything is sent.
        if !info.in_headers {
            body.push(' ');
            body.push_str(&i18n(
                "This message carries no unsubscribe header, so Hylki is going by the link in it.",
            ));
        }
        let parent = relm4::main_application()
            .active_window()
            .unwrap_or_else(|| self.window.clone().upcast());
        let dialog = adw::MessageDialog::new(Some(&parent), Some(&heading), Some(body.as_str()));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("unsub", &i18n("Unsubscribe"));
        dialog.set_default_response(Some("unsub"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("unsub", adw::ResponseAppearance::Suggested);
        // The mail already received from the list's address can go with it
        // (#407). Off unless ticked: the address may send other mail too.
        let n = self
            .mail_from_address(message.account_id, &message.from_addr)
            .len();
        let delete_too = (n > 0).then(|| {
            let check = gtk::CheckButton::with_label(&ni18n_f(
                "Also delete the message from {addr}",
                "Also delete the {n} messages from {addr}",
                n as u32,
                &[("n", &n.to_string()), ("addr", &message.from_addr)],
            ));
            check.set_halign(gtk::Align::Center);
            dialog.set_extra_child(Some(&check));
            check
        });
        let s = sender.clone();
        let message = Box::new(message);
        let info = Box::new(info);
        dialog.connect_response(None, move |_, resp| {
            if resp == "unsub" {
                s.input(AppMsg::UnsubscribeGo {
                    message: message.clone(),
                    info: info.clone(),
                });
                if delete_too.as_ref().is_some_and(|c| c.is_active()) {
                    s.input(AppMsg::DeleteFromAddress {
                        account_id: message.account_id,
                        addr: message.from_addr.clone(),
                    });
                }
            }
        });
        dialog.present();
        // HYLKI_SHOWCASE_UNSUB=ask: the dialog is its own surface, so the
        // main window's capture never shows it — take one of it too.
        if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok_and(|v| v == "ask") {
            if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                let d = dialog.clone();
                gtk::glib::timeout_add_seconds_local_once(1, move || {
                    showcase_capture(d.upcast_ref::<gtk::Widget>(), &format!("{path}.dialog.png"));
                });
            }
        }
    }

    /// Leave the list, by the best route it offers: the one-click POST
    /// (RFC 8058), falling back to its `mailto:` handle when that fails or
    /// is not there, and only then the web page in the browser.
    fn unsubscribe_go(
        &self,
        message: Message,
        info: crate::models::Unsubscribe,
        sender: &ComponentSender<Self>,
    ) {
        let key = (message.account_id, message.id);
        self.push_unsubscribe_state(key, Some(UnsubState::Working));
        let message = Box::new(message);
        if demo_mode() {
            // The demo has no list to ask: the request succeeds after a beat
            // (or fails, for a look at that banner: HYLKI_SHOWCASE_UNSUB=fail).
            let s = sender.clone();
            let fail = std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok_and(|v| v == "fail");
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
                let result = if fail {
                    Err("the list's server answered 503".to_string())
                } else {
                    Ok(true)
                };
                s.input(AppMsg::UnsubscribeDone { message, result });
            });
            return;
        }
        if let Some(url) = info.one_click.clone() {
            let s = sender.clone();
            let info = Box::new(info);
            std::thread::spawn(
                move || match crate::mail::unsubscribe::one_click_post(&url) {
                    Ok(()) => s.input(AppMsg::UnsubscribeDone {
                        message,
                        result: Ok(true),
                    }),
                    Err(why) if info.mailto.is_some() || info.reply.is_some() => {
                        tracing::warn!(
                            "one-click unsubscribe failed ({why}); writing to the list instead"
                        );
                        s.input(AppMsg::UnsubscribeByMail { message, info });
                    }
                    // The one-click handle is the list's web page as well: a
                    // POST it refused (a redirect, most often) may still work
                    // in the browser, which is the last route anyway.
                    Err(why) if info.web.is_some() => {
                        tracing::warn!(
                            "one-click unsubscribe failed ({why}); opening the list's page instead"
                        );
                        let info = Box::new(crate::models::Unsubscribe {
                            one_click: None,
                            ..*info
                        });
                        s.input(AppMsg::UnsubscribeGo { message, info });
                    }
                    Err(why) => s.input(AppMsg::UnsubscribeDone {
                        message,
                        result: Err(why),
                    }),
                },
            );
            return;
        }
        if info.mailto.is_some() || info.reply.is_some() {
            self.unsubscribe_by_mail(*message, &info, sender);
            return;
        }
        match info.web.as_deref() {
            Some(url) => {
                crate::ui::launch::open_link(url, Some(self.window.upcast_ref()));
                sender.input(AppMsg::UnsubscribeDone {
                    message,
                    result: Ok(false),
                });
            }
            None => sender.input(AppMsg::UnsubscribeDone {
                message,
                result: Err(i18n("the message offers no way to unsubscribe")),
            }),
        }
    }

    /// The `mailto:` route: a short message to the list's handle, sent
    /// through the account the message arrived in (as the alias it was
    /// addressed to, when it was one). No copy is filed in Sent. A send that
    /// fails is reported and queued in the Outbox like any other.
    fn unsubscribe_by_mail(
        &self,
        message: Message,
        info: &crate::models::Unsubscribe,
        sender: &ComponentSender<Self>,
    ) {
        let Some(target) = Self::unsubscribe_mail_target(info) else {
            sender.input(AppMsg::UnsubscribeDone {
                message: Box::new(message),
                result: Err(i18n("the list's mail address could not be read")),
            });
            return;
        };
        let (from_alias, _) = self.unsubscribe_from(&message);
        let out = machine_message(
            self.sending_account(&message),
            from_alias,
            target.to,
            target.subject,
            target.body,
            // A "reply with UNSUBSCRIBE" is a reply: the list matches it to
            // the message it sent. A `mailto:` handle is a fresh message.
            if info.mailto.is_some() {
                String::new()
            } else {
                message.message_id.clone()
            },
        );
        self.send_unfiled(out);
        sender.input(AppMsg::UnsubscribeDone {
            message: Box::new(message),
            result: Ok(true),
        });
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_unsubscribe(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Unsubscribe { message, info } => {
                if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok_and(|v| v == "go" || v == "fail") {
                    self.unsubscribe_go(*message, *info, &sender);
                } else {
                    self.confirm_unsubscribe(*message, *info, &sender);
                }
            }
            AppMsg::UnsubscribeGo { message, info } => {
                self.unsubscribe_go(*message, *info, &sender);
            }
            AppMsg::UnsubscribeByMail { message, info } => {
                self.unsubscribe_by_mail(*message, &info, &sender);
            }
            AppMsg::UnsubscribeDone { message, result } => {
                let key = (message.account_id, message.id);
                match result {
                    Ok(left) => {
                        if left {
                            self.record_unsubscribed(&message);
                            self.notice(i18n_f(
                                "Unsubscribed from {name}.",
                                &[("name", &sender_label(&message))],
                            ));
                        } else {
                            self.notice(i18n(
                                "The list's unsubscribe page is open in your browser.",
                            ));
                        }
                        self.push_unsubscribe_state(key, None);
                    }
                    Err(why) => {
                        tracing::warn!("unsubscribe failed for {}: {why}", message.from_addr);
                        self.push_unsubscribe_state(key, Some(UnsubState::Failed(why)));
                    }
                }
            }
            _ => unreachable!("not routed to on_unsubscribe"),
        }
    }
}
