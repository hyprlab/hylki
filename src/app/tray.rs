//! The tray icon and its menu, and the launcher badge.

use std::collections::HashSet;

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n;
use crate::models::{Account, Message};
use crate::ui::sidebar::SidebarInput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::window::exit_now;

mod indicator;

pub(super) use indicator::TrayHandle;

/// What identifies a tray mail list for change detection: the total, then
/// each card.
fn tray_mail_key(mail: &indicator::TrayMailList) -> Vec<(u32, u32, String, String)> {
    std::iter::once((u32::MAX, mail.unread, String::new(), String::new()))
        .chain(mail.items.iter().map(|m| {
            (
                m.account_id,
                m.message_id,
                m.heading.clone(),
                m.subject.clone(),
            )
        }))
        .collect()
}

impl AppModel {
    /// The folders whose unread count has grown since they were last
    /// looked at (#343). A folder on screen is being looked at, so its mark
    /// follows its count; elsewhere the mark only comes down, as mail is
    /// read. A folder never seen before starts with what it has, so turning
    /// the setting on, or a new account, marks nothing.
    fn fresh_unread(&self) -> HashSet<(u32, u32)> {
        let on_screen = |a: u32, f: u32| {
            self.window.is_visible()
                && if self.unified {
                    self.is_unified_target(a, f)
                } else {
                    self.selected
                        .as_ref()
                        .is_some_and(|s| s.account_id == a && s.folder_id == f)
                }
        };
        let mut seen = self.unread_seen.borrow_mut();
        let mut changed = false;
        let mut fresh = HashSet::new();
        for (&(a, f), &n) in &self.folder_unread {
            let (Some(email), Some(path)) = (self.email_of(a), self.folder_path(a, f)) else {
                continue;
            };
            let key = format!("{email}\u{1f}{path}");
            let mark = seen.get(&key).copied();
            let next = match mark {
                _ if on_screen(a, f) => n,
                None => n,
                Some(m) => m.min(n),
            };
            if mark != Some(next) {
                seen.insert(key, next);
                changed = true;
            }
            if n > next {
                fresh.insert((a, f));
            }
        }
        if changed {
            config::save_unread_seen(&seen);
        }
        fresh
    }

    /// `push_unread_counts` once the view switch has been drawn: it walks
    /// every folder, and the counts can wait a frame where the list cannot.
    pub(super) fn push_unread_counts_later(&self, sender: &ComponentSender<Self>) {
        let s = sender.input_sender().clone();
        gtk::glib::idle_add_local_once(move || {
            let _ = s.send(AppMsg::PushUnreadCounts);
        });
    }

    /// Update the sidebar's unread badges in place (no rebuild), derived from the
    /// loaded message lists. Cheap enough to call on every read/sync.
    pub(super) fn push_unread_counts(&self) {
        let folders = self.folder_unread.clone();
        let unified = self.unified_unread();
        let fresh = self.seen_counts.then(|| self.fresh_unread());
        self.sidebars_emit(SidebarInput::SetUnread {
            folders,
            unified: self.unified_inboxes_unread(),
            fresh,
        });
        // The counted total is what GNOME shows beside Hylki in Background
        // Apps, so a process with no window still says what it is there for.
        if self.run_in_background.get() {
            crate::desktop::background::set_status(&crate::desktop::background::status_text(
                unified,
            ));
        }
        // And what the tray icon's dot answers to: the inboxes alone, as
        // its menu says.
        if let Some(tray) = &self.tray {
            tray.set_unread(self.inboxes_unread());
        }
        self.push_launcher_count();
        self.push_tray_mail();
    }

    /// The launcher icon's badge (#271): the same inboxes total as the tray
    /// icon's dot, only the inbox mail that came in while the window was
    /// behind (#431), or nothing, as Settings has it.
    pub(super) fn push_launcher_count(&self) {
        let count = match self.launcher_badge {
            config::LauncherBadge::Off => 0,
            config::LauncherBadge::AllUnread => self.inboxes_unread(),
            config::LauncherBadge::NewSinceFront => self.arrivals.count(|a, f| {
                self.message_cache
                    .get(&(a, f))
                    .into_iter()
                    .flatten()
                    .filter(|m| m.unread)
                    .map(|m| m.uid)
            }),
        };
        crate::desktop::launcher_badge::set_count(count);
    }

    /// Send the launcher badge again even though the count is unchanged, for
    /// when the dock may have dropped it (a lock, a wake).
    pub(super) fn resend_launcher_count(&self) {
        crate::desktop::launcher_badge::resend();
        self.push_launcher_count();
    }

    /// Publish the tray item with the current icon, unread total and mail list.
    pub(super) fn start_tray(&mut self, sender: &ComponentSender<Self>) {
        let unified = self.unified_unread();
        let mail = self.tray_mail.then(|| self.tray_mail_list());
        *self.tray_mail_key.borrow_mut() = mail.as_ref().map(tray_mail_key);
        self.tray = indicator::TrayHandle::start(
            sender.input_sender().clone(),
            self.tray_icon,
            &crate::desktop::app_icon::tray_image(&self.app_icon),
            unified,
            mail,
        );
    }

    /// The window a dialog about the icon change belongs over: Settings
    /// while it is open, else the main window.
    fn icon_dialog_parent(&self) -> gtk::Window {
        match self.prefs.as_ref().filter(|p| p.widget().is_visible()) {
            Some(p) => p.widget().clone().upcast(),
            None => self.window.clone().upcast(),
        }
    }

    /// While a restart waits for the desktop to take the launcher in: a
    /// modal with a spinner, so the pause reads as progress rather than a
    /// stuck app. It has no way to close — the app quits from under it.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn show_restarting_dialog(&self) {
        let dialog = adw::MessageDialog::new(
            Some(&self.icon_dialog_parent()),
            Some(i18n("Restarting Hylki").as_str()),
            Some(
                i18n("Applying your new app icon. Hylki will close and reopen in a moment.")
                    .as_str(),
            ),
        );
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_margin_top(6);
        spinner.start();
        dialog.set_extra_child(Some(&spinner));
        dialog.present();
    }

    /// The heads-up after an icon change. The launcher already carries the
    /// new icon, so the app grid shows it — but GNOME Shell keeps a running
    /// app's windows bound to the app object it created for the old
    /// launcher (a changed launcher only replaces the object, it never
    /// re-tracks the windows), so the dock's running entry, like Hylki's
    /// own window icon, only switches once every window has closed: a
    /// restart. Offered, never forced, over whichever window the change
    /// came from.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    pub(super) fn offer_restart_for_icon(&self, sender: &ComponentSender<Self>) {
        let dialog = adw::MessageDialog::new(
            Some(&self.icon_dialog_parent()),
            Some(i18n("Restart to finish switching icons?").as_str()),
            Some(
                i18n("The dock and Hylki's own windows keep the old icon until Hylki restarts.")
                    .as_str(),
            ),
        );
        dialog.add_responses(&[
            ("later", i18n("Later").as_str()),
            ("restart", i18n("Restart Now").as_str()),
        ]);
        dialog.set_response_appearance("restart", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("later"));
        dialog.set_close_response("later");
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "restart" {
                s.input(AppMsg::RestartApp);
            }
        });
        dialog.present();
    }

    /// Send the tray menu its mail list when it has changed.
    pub(super) fn push_tray_mail(&self) {
        let Some(tray) = &self.tray else { return };
        let mail = self.tray_mail.then(|| self.tray_mail_list());
        let key = mail.as_ref().map(tray_mail_key);
        if *self.tray_mail_key.borrow() == key {
            return;
        }
        *self.tray_mail_key.borrow_mut() = key;
        tray.set_mail(mail);
    }

    /// The newest unread messages across accounts' inboxes, as tray cards,
    /// with the inboxes' unread total (the cards may be fewer). Filter
    /// destinations are left out: the menu says it lists inboxes.
    fn tray_mail_list(&self) -> indicator::TrayMailList {
        let unread = self.inboxes_unread();
        let items = self.tray_mail_cards();
        indicator::TrayMailList { items, unread }
    }

    fn tray_mail_cards(&self) -> Vec<indicator::TrayMail> {
        let several = self.accounts.len() > 1;
        let mut unread: Vec<(&Account, &Message)> = self
            .accounts
            .iter()
            .filter_map(|a| self.inbox_of(a.id).map(|f| (a, f)))
            .flat_map(|(a, f)| {
                self.message_cache
                    .get(&(a.id, f.id))
                    .into_iter()
                    .flat_map(|msgs| msgs.iter())
                    .filter(|m| m.unread)
                    .map(move |m| (a, m))
            })
            .collect();
        unread.sort_by_key(|x| std::cmp::Reverse(x.1.timestamp));
        unread
            .into_iter()
            .take(indicator::MAIL_LIMIT)
            .map(|(a, m)| {
                let sender = crate::contacts::shown_name_or_addr(&m.from_name, &m.from_addr);
                let mut heading = indicator::clip(&sender, 40);
                if several {
                    heading.push_str(" · ");
                    heading.push_str(&indicator::clip(&a.label, 24));
                }
                if !m.date.is_empty() {
                    heading.push_str(" · ");
                    heading.push_str(&m.date);
                }
                let texture = match crate::services::avatar::lookup(&m.from_addr, self.gravatar) {
                    crate::services::avatar::CacheLookup::Texture(t) => Some(t),
                    _ => None,
                };
                indicator::TrayMail {
                    account_id: m.account_id,
                    folder_id: m.folder_id,
                    message_id: m.id,
                    heading,
                    subject: indicator::clip(&m.subject, 60),
                    preview: indicator::clip(&m.preview, 70),
                    icon: indicator::sender_icon(&m.shown_from_name(), &m.from_addr, texture),
                }
            })
            .collect()
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_tray(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::WizardLanguage(code) => {
                // The drop-down also notifies as it is set up; only a real
                // change counts. Saved, then Hylki restarts through the same
                // helper the icon change uses, so every window — the wizard
                // first, since it is not completed yet — comes up in the
                // chosen language. Should the helper fail, the wizard alone
                // is rebuilt in it (gettext reads LANGUAGE on every lookup).
                if config::load_language() == code {
                    return;
                }
                config::save_language(&code);
                tracing::info!("wizard: language {code:?} saved, restarting");
                // The instance that comes back must open the wizard again,
                // review mode or not: the helper drops the review switch on
                // purpose, so a one-shot flag of its own goes along (init
                // takes it and clears it, so it never outlives that start).
                std::env::set_var(crate::WIZARD_AGAIN_VAR, "1");
                match crate::desktop::app_icon::launch_restart_helper() {
                    Ok(()) => {
                        // The quit action's own teardown, inline: nothing to
                        // look up, nothing to wait for.
                        tracing::info!("wizard: restart helper up, exiting");
                        let (w, h, maximized) = crate::config::load_window_state();
                        crate::config::save_window_state(w, h, maximized);
                        exit_now(&self.outgoing);
                    }
                    Err(e) => {
                        std::env::remove_var(crate::WIZARD_AGAIN_VAR);
                        tracing::warn!("restart for the wizard's language failed: {e}");
                        crate::i18n::apply_language(&code);
                        if let Some(old) = self.welcome.take() {
                            old.widget().set_visible(false);
                        }
                        self.open_wizard(&sender);
                    }
                }
            }

            AppMsg::RestartApp => {
                if self.restart_pending {
                    return;
                }
                self.restart_pending = true;
                // GNOME Shell only replaces a launcher's app object some
                // seconds after the launcher changed; quitting before that
                // brings the new window up under the old one, old icon and
                // all. Wait it out with the window still up.
                let wait = crate::desktop::app_icon::launcher_settle_remaining();
                self.show_restarting_dialog();
                if wait.is_zero() {
                    sender.input(AppMsg::RestartNow);
                } else {
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(wait, move || s.input(AppMsg::RestartNow));
                }
            }

            AppMsg::RestartNow => match crate::desktop::app_icon::launch_restart_helper() {
                Ok(()) => relm4::main_application().activate_action("quit", None),
                Err(e) => {
                    tracing::warn!("restart helper failed: {e}");
                    self.restart_pending = false;
                    self.notice_error(i18n(
                        "Couldn't restart automatically — quit and reopen Hylki \
                               to finish switching the icon.",
                    ));
                }
            },

            AppMsg::QuitFromTray => {
                // The same teardown as Ctrl+Q: geometry saved, then exit.
                relm4::main_application().activate_action("quit", None);
            }

            AppMsg::TrayViewUnread => {
                // The tray counts every inbox (#267): when all of its unread
                // mail is in accounts left out of All Inboxes, that view
                // would show none of it.
                let unified = self.unified_inboxes_shown()
                    && (self.unified_inboxes_unread() > 0 || self.inboxes_unread() == 0);
                if unified {
                    // Through the sidebar, so its highlight moves too; it
                    // answers with UnifiedSelected.
                    self.sidebar.emit(SidebarInput::SelectUnifiedRow);
                } else if let Some((account_id, folder_id, name, path)) = self
                    .accounts
                    .iter()
                    .filter_map(|a| self.inbox_of(a.id))
                    .find(|f| self.folder_unread_of(f) > 0)
                    .map(|f| (f.account_id, f.id, f.name.clone(), f.path.clone()))
                {
                    self.select_folder(account_id, folder_id, name, path);
                }
            }

            AppMsg::PresentWindow => {
                crate::desktop::session::present_window(&self.window);
            }
            _ => unreachable!("not routed to on_tray"),
        }
    }
}
