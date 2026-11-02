//! Desktop notifications for new mail and what their buttons do.

use adw::prelude::*;
use relm4::prelude::*;

use crate::models::{Folder, FolderKind, Message};
use crate::ui::message_list::MessageListInput;
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::demo::demo_mode;
use super::flags::read_label;
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::reply_prefill;
use super::types::UnifiedView;
use super::undo::FlagKind;

/// Resolve a notification's `(account_id, folder_id, message_id)` to the full
/// cached [`Message`] (#38): the notified message may not be in the current
/// view, but the worker upserts it into the cache before notifying, so the
/// cache always has it.
fn notified_message(
    account_id: u32,
    folder_id: u32,
    message_id: u32,
    folders: &std::collections::HashMap<u32, Vec<Folder>>,
) -> Option<Message> {
    let path = folders
        .get(&account_id)?
        .iter()
        .find(|f| f.id == folder_id)?
        .path
        .clone();
    let cache = crate::cache::Cache::open().ok()?;
    cache
        .load_messages(account_id, &path, folder_id)
        .into_iter()
        .find(|m| m.id == message_id)
}

impl AppModel {
    /// Shows a passing message in the status bar.
    pub(super) fn notice(&self, text: String) {
        self.notifications.emit(NotifyInput::Push {
            text,
            error: false,
            connectivity: false,
        });
    }

    /// Shows an error in the status bar, kept as a card in its panel.
    pub(super) fn notice_error(&self, text: String) {
        self.notifications.emit(NotifyInput::Push {
            text,
            error: true,
            connectivity: false,
        });
    }

    /// Tell the person about new Inbox mail: the notification, with its
    /// sound, while the window is in the background or closed; the sound
    /// alone while it is in front, when Settings asks for that (#337).
    /// GNOME shows no banner for the app in front, and neither does Hylki.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn announce_new_mail(
        &self,
        account_id: u32,
        folder_id: u32,
        message_id: u32,
        from: &str,
        subject: &str,
        others: usize,
        in_place: bool,
    ) {
        // HYLKI_SHOWCASE_IN_FRONT (demo) stands in for the focus, which a
        // window opened from a script never gets.
        let in_front = self.window.is_active()
            || (demo_mode() && std::env::var_os("HYLKI_SHOWCASE_IN_FRONT").is_some());
        if !in_front {
            crate::desktop::notify::new_mail(
                account_id, folder_id, message_id, from, subject, others, in_place,
            );
        } else if crate::config::sound_when_focused() {
            tracing::info!("new mail with the window in front: the sound alone");
            crate::desktop::notify::sound_for_new_mail();
        } else {
            tracing::debug!("new mail with the window in front: no notification, no sound");
        }
    }

    pub(super) fn show_toast(&self, toast: &adw::Toast) {
        if let Some(t) = self.toasts.get() {
            t.add_toast(toast.clone());
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_notifications(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Notice(text) => {
                self.notice(text);
            }

            AppMsg::OpenMessageFromNotification {
                account_id,
                folder_id,
                message_id,
            } => self.open_from_notification(account_id, folder_id, message_id, sender),

            AppMsg::NotificationMarkRead {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // set_read clears the account's notification itself.
                    self.user_set_flags(read_label(true), &[m], FlagKind::Read, true);
                }
            }

            AppMsg::NotificationArchive {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.move_to(m, FolderKind::Archive);
                    crate::desktop::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationDelete {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // The same path as the Delete key: to Trash, with the
                    // usual undo; a message already there asks first.
                    self.delete_messages(vec![m], &sender);
                    crate::desktop::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationSpam {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.mark_spam_msg(m);
                    crate::desktop::notify::withdraw_mail(account_id);
                }
            }

            // The message is opening in the reader (the action sent
            // OpenMessageFromNotification first); the composer waits for
            // the body it quotes when that is still on its way.
            AppMsg::NotificationReply {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    let m = self.with_cached_body(m);
                    if m.body.is_empty() {
                        self.pending_notified_reply = Some(m);
                    } else {
                        self.open_reply(
                            m.account_id,
                            self.reply_context(&m, reply_prefill(&m, &self.reply_header)),
                            (m.account_id, m.id),
                            &sender,
                        );
                    }
                }
            }

            AppMsg::NotificationForward {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.forward(m, true, &sender);
                }
            }

            AppMsg::Status { account_id, text } => {
                if text.is_empty() {
                    self.busy.remove(&account_id);
                } else {
                    self.busy.insert(account_id);
                }
                self.update_busy_indicator();
                // A sync going quiet must not blank the status while background
                // bulk operations are still reporting there, and hands it back
                // to the copying to On This Computer when that is still going.
                if !text.is_empty() {
                    self.notifications.emit(NotifyInput::SetStatus(text));
                } else if self.bulk_pending == 0 {
                    self.notifications
                        .emit(NotifyInput::SetStatus(self.copy_status()));
                }
            }

            AppMsg::CopyProgress { left } => {
                self.copies_left = left;
                self.update_busy_indicator();
                // A sync's own line, or a bulk operation's, comes first.
                if self.busy.is_empty() && self.bulk_pending == 0 {
                    self.notifications
                        .emit(NotifyInput::SetStatus(self.copy_status()));
                }
            }

            AppMsg::Error {
                account_id,
                text,
                connectivity,
            } => {
                // The log gets the whole error (a parser dump included, which
                // is what diagnoses #226-style replies); the bar gets it
                // readable.
                tracing::error!("[account {account_id}] {text}");
                // A connection that died with the network: the Refresh
                // button already says it is down (#442).
                if connectivity && !crate::desktop::power::reaches_internet() {
                    return;
                }
                let text = crate::worker::readable_error(&text);
                let label = self.account_label(account_id);
                // Desktop-notify only genuine failures (not transient connectivity
                // blips that auto-recover), and only when unfocused — the in-app bar
                // already surfaces it while you're looking.
                if self.notifications_enabled && !connectivity && !self.window.is_active() {
                    crate::desktop::notify::error(
                        account_id,
                        &format!("{label}: mail error"),
                        &text,
                    );
                }
                self.notifications.emit(NotifyInput::Push {
                    text: format!("{label}: {text}"),
                    error: true,
                    connectivity,
                });
            }

            AppMsg::DirectoryError(text) => {
                self.notice_error(text);
            }

            AppMsg::NotifyCount(n) => self.notify_count = n,
            AppMsg::ToggleNotifications => self.notifications.emit(NotifyInput::ToggleBar),
            AppMsg::StatusBarShown(shown) => {
                self.status_bar_shown = shown;
                self.rebuild_help_menu();
            }
            _ => unreachable!("not routed to on_notifications"),
        }
    }

    /// A new-mail desktop notification was clicked — open that message.
    fn open_from_notification(
        &mut self,
        account_id: u32,
        folder_id: u32,
        message_id: u32,
        sender: ComponentSender<Self>,
    ) {
        // The user clicked a new-mail notification: they've engaged with
        // that account's mail, so clear its toast, then navigate to the
        // message's folder and open it in the reader.
        crate::desktop::notify::withdraw_mail(account_id);
        tracing::info!(
            "notification: open account {account_id} folder {folder_id} message {message_id} (unified row: {})",
            self.unified_inboxes_shown()
        );
        if let Some((name, path, kind)) = self
            .folder_by_id(account_id, folder_id)
            .map(|f| (f.name.clone(), f.path.clone(), f.kind))
        {
            // Ask for the body before anything else goes to the
            // account's worker. Opening Inboxes below asks every
            // account for its inbox list, and the worker serves one
            // request at a time: the body request the selection
            // sends afterwards would wait behind that whole fetch
            // (and the IDLE hand-off before it), so the message the
            // user clicked sat on a spinner while the list loaded.
            // Sent first, it comes back into the body cache, which
            // the selection reads before fetching. A body already
            // there (prefetched on arrival) needs nothing.
            let uid = self
                .message_cache
                .get(&(account_id, folder_id))
                .and_then(|msgs| msgs.iter().find(|m| m.id == message_id))
                .map(|m| m.uid);
            if let Some(uid) = uid {
                if !self.body_cache.contains_key(&(account_id, folder_id, uid)) {
                    self.send_to(
                        account_id,
                        MailRequest::LoadBody {
                            message_id,
                            path: path.clone(),
                            uid,
                        },
                    );
                }
            }
            // Mail that landed in an inbox opens in the unified
            // Inboxes when the sidebar has that row (the view the
            // app opens with; the account's own Inbox row may sit
            // inside a folded section, where a highlight has nowhere
            // to show). Without the row — one account, or the
            // preference off — or when a filter filed the message
            // elsewhere, its folder opens directly and the sidebar
            // unfolds the account to show where that is. Both emit
            // the (cached) list synchronously, so the SelectAndLoad
            // that follows finds the row and opens it.
            if kind == FolderKind::Inbox
                && self.unified_inboxes_shown()
                && self.in_unified(account_id)
            {
                self.open_unified(UnifiedView::Kind(FolderKind::Inbox), &sender);
            } else {
                self.select_folder(account_id, folder_id, name, path);
            }
            self.message_list
                .emit(MessageListInput::SelectAndLoad((account_id, message_id)));
        }
    }

    /// Desktop-notification click actions: raise the window (error alerts) and
    /// raise + open a specific message (new-mail alerts). Registered here rather
    /// than in `notify` because opening a message needs the app's channel.
    pub(super) fn install_notification_actions(&self, sender: &ComponentSender<Self>) {
        self.install_present_actions(sender);
        let ty = &*<(u32, u32, u32)>::static_variant_type();
        self.install_open_message_action(sender, ty);
        install_quiet_message_actions(sender, ty);
        self.install_windowed_message_actions(sender, ty);
    }

    /// Present (a click on an alert) and the "message ready" alerts'
    /// own present, which raises the composer too.
    fn install_present_actions(&self, sender: &ComponentSender<Self>) {
        let app = relm4::main_application();
        let present = gtk::gio::SimpleAction::new(crate::desktop::notify::PRESENT_ACTION, None);
        let win = self.window.clone();
        present.connect_activate(move |_, _| {
            crate::desktop::session::present_window(&win);
        });
        app.add_action(&present);

        // "Message ready" alerts (a hand-off whose window stayed behind):
        // the click raises the composer too, and the alert goes away by
        // itself once any window of ours has the focus.
        let present_compose =
            gtk::gio::SimpleAction::new(crate::desktop::notify::PRESENT_COMPOSE_ACTION, None);
        let psender = sender.clone();
        present_compose.connect_activate(move |_, _| {
            psender.input(AppMsg::PresentComposers);
        });
        app.add_action(&present_compose);
        let fsender = sender.clone();
        self.window.connect_is_active_notify(move |w| {
            if w.is_active() {
                crate::desktop::notify::withdraw_compose_ready();
                // Focus comes back after an unlock, when the dock has
                // just been rebuilt without the badge; and the mail
                // that arrived meanwhile has now been seen (#431).
                fsender.input(AppMsg::WindowInFront);
            }
        });
    }

    /// A click on a new-mail alert: the window comes up on that message.
    fn install_open_message_action(
        &self,
        sender: &ComponentSender<Self>,
        ty: &gtk::glib::VariantTy,
    ) {
        let app = relm4::main_application();
        let open =
            gtk::gio::SimpleAction::new(crate::desktop::notify::OPEN_MESSAGE_ACTION, Some(ty));
        let win = self.window.clone();
        let osender = sender.clone();
        open.connect_activate(move |_, param| {
            crate::desktop::session::present_window(&win);
            if let Some((account_id, folder_id, message_id)) =
                param.and_then(|v| v.get::<(u32, u32, u32)>())
            {
                osender.input(AppMsg::OpenMessageFromNotification {
                    account_id,
                    folder_id,
                    message_id,
                });
            }
        });
        app.add_action(&open);
    }

    /// The alert buttons that need the window: Reply and Forward.
    fn install_windowed_message_actions(
        &self,
        sender: &ComponentSender<Self>,
        ty: &gtk::glib::VariantTy,
    ) {
        let app = relm4::main_application();
        // Reply and Forward (#244) need the window: the message opens
        // as a click on the notification would open it, and the
        // composer follows once its body is in.
        for (name, mk) in [
            (
                crate::desktop::notify::REPLY_ACTION,
                (&|account_id, folder_id, message_id| AppMsg::NotificationReply {
                    account_id,
                    folder_id,
                    message_id,
                }) as &dyn Fn(u32, u32, u32) -> AppMsg,
            ),
            (
                crate::desktop::notify::FORWARD_ACTION,
                &|account_id, folder_id, message_id| AppMsg::NotificationForward {
                    account_id,
                    folder_id,
                    message_id,
                },
            ),
        ] {
            let act = gtk::gio::SimpleAction::new(name, Some(ty));
            let win = self.window.clone();
            let asender = sender.clone();
            act.connect_activate(move |_, param| {
                crate::desktop::session::present_window(&win);
                if let Some((account_id, folder_id, message_id)) =
                    param.and_then(|v| v.get::<(u32, u32, u32)>())
                {
                    asender.input(AppMsg::OpenMessageFromNotification {
                        account_id,
                        folder_id,
                        message_id,
                    });
                    asender.input(mk(account_id, folder_id, message_id));
                }
            });
            app.add_action(&act);
        }
    }

    /// GNOME keeps a notification after the app that posted it has quit,
    /// and a new instance knows nothing of what it pointed at, so it
    /// could only linger (#333). New mail since is notified afresh.
    pub(super) fn withdraw_stale_notifications(&self) {
        for a in &self.accounts {
            crate::desktop::notify::withdraw_mail(a.id);
        }
    }
}

/// The alert buttons that act on the message where it lies.
fn install_quiet_message_actions(sender: &ComponentSender<AppModel>, ty: &gtk::glib::VariantTy) {
    let app = relm4::main_application();
    // Notification buttons (#38): act on the message where it lies,
    // without presenting the window — that's their point.
    for (name, mk) in [
        (
            crate::desktop::notify::MARK_READ_ACTION,
            (&|account_id, folder_id, message_id| AppMsg::NotificationMarkRead {
                account_id,
                folder_id,
                message_id,
            }) as &dyn Fn(u32, u32, u32) -> AppMsg,
        ),
        (
            crate::desktop::notify::ARCHIVE_ACTION,
            &|account_id, folder_id, message_id| AppMsg::NotificationArchive {
                account_id,
                folder_id,
                message_id,
            },
        ),
        (
            crate::desktop::notify::DELETE_ACTION,
            &|account_id, folder_id, message_id| AppMsg::NotificationDelete {
                account_id,
                folder_id,
                message_id,
            },
        ),
        (
            crate::desktop::notify::SPAM_ACTION,
            &|account_id, folder_id, message_id| AppMsg::NotificationSpam {
                account_id,
                folder_id,
                message_id,
            },
        ),
    ] {
        let act = gtk::gio::SimpleAction::new(name, Some(ty));
        let asender = sender.clone();
        act.connect_activate(move |_, param| {
            // No window will spend the shell's launch token, so
            // end the launch here, or the busy pointer stays up.
            crate::desktop::startup::complete();
            if let Some((account_id, folder_id, message_id)) =
                param.and_then(|v| v.get::<(u32, u32, u32)>())
            {
                asender.input(mk(account_id, folder_id, message_id));
            }
        });
        app.add_action(&act);
    }
}

impl AppModel {
    /// The status line while accounts copy to On This Computer (#552);
    /// empty when nothing is left. What a finished task leaves the line
    /// showing.
    pub(super) fn copy_status(&self) -> String {
        match self.copies_left {
            0 => String::new(),
            n => crate::i18n::ni18n_f(
                "Copying one message to On This Computer…",
                "Copying {n} messages to On This Computer…",
                n as u32,
                &[("n", &n.to_string())],
            ),
        }
    }
}
