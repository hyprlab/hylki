//! Applying general and appearance settings.

use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::notifications::NotifyInput;
use crate::ui::preferences::PrefOutput;

use super::attachments::prune_downloads;
use super::model::AppModel;
use super::theme::{apply_app_theme, refresh_scheme_css};
use crate::desktop::icon_theme::apply_icon_theme;

impl AppModel {
    /// Push the date and clock preference into the formatter and redraw whatever
    /// shows a date: every row carries one, as does the open message.
    fn apply_date_style(&mut self) {
        crate::datefmt::set_style(self.date_style, self.clock_style);
        self.message_list.emit(MessageListInput::RefreshDates);
        let current = self.current.clone();
        self.show_message(current, false);
    }

    /// The settings changes the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_general_pref(
        &mut self,
        pref: PrefOutput,
        sender: ComponentSender<Self>,
    ) {
        match pref {
            PrefOutput::SetAppTheme(theme) => {
                if self.app_theme != theme {
                    self.app_theme = theme;
                    apply_app_theme(theme);
                    self.save_settings();
                }
            }

            PrefOutput::SetTextScale(percent) => {
                if self.text_scale != percent {
                    self.text_scale = percent;
                    crate::desktop::text_scale::apply(percent);
                    self.save_settings();
                }
            }

            PrefOutput::SetTheme(id) => {
                if self.theme != id {
                    self.theme = id.clone();
                    // Repaints the chrome and tells the reader and any open
                    // composer to re-ground their documents. The colors the
                    // scheme-dependent CSS reads back are taken a main-loop
                    // pass later, as they are on a light/dark flip, so the
                    // lookups answer for the palette that just landed.
                    crate::desktop::theme::set(&id);
                    gtk::glib::idle_add_local_once(refresh_scheme_css);
                    self.save_settings();
                }
            }

            PrefOutput::SetAvatars(on) => {
                if pref!(self.avatars = on) {
                    self.push_list_look(false);
                }
            }

            PrefOutput::SetFocusMode(focus) => self.set_focus_mode(focus),
            PrefOutput::SetTagRowTint(on) => {
                if pref!(self.tag_row_tint = on) {
                    self.refresh_tag_css();
                }
            }
            PrefOutput::SetOwnMailboxFace(on) => {
                if pref!(self.own_mailbox_face = on) {
                    self.refresh_own_faces();
                    // Switched on, the accounts that want a Gravatar may never
                    // have been asked about (the switch was off at startup).
                    if on {
                        self.warm_own_gravatars(&sender);
                    }
                    self.refresh_faces();
                }
            }

            PrefOutput::SetSenderLogos(on) => {
                if pref!(self.sender_logos = on) {
                    self.message_list.emit(MessageListInput::SetSenderLogos(on));
                }
            }

            PrefOutput::SetDateStyle(style) => {
                if pref!(self.date_style = style) {
                    self.apply_date_style();
                }
            }

            PrefOutput::SetRenderer(renderer) => {
                // The combo notifies as its model is set too: only a real
                // change is saved and announced.
                if config::load_renderer() != renderer {
                    config::save_renderer(&renderer);
                    self.notice(i18n("The renderer applies the next time Hylki starts."));
                }
            }

            PrefOutput::SetLanguage(code) => {
                // The combo also notifies as its model is set; only a real
                // change is saved and announced.
                if config::load_language() != code {
                    config::save_language(&code);
                    self.notice(i18n("The language applies the next time Hylki starts."));
                }
            }

            PrefOutput::SetClockStyle(style) => {
                if pref!(self.clock_style = style) {
                    self.apply_date_style();
                }
            }

            PrefOutput::SetGravatar(on) => {
                if pref!(self.gravatar = on) {
                    self.message_list.emit(MessageListInput::SetGravatar(on));
                    // Refresh the reader's avatar for the open message.
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
            }

            PrefOutput::SetFetchInterval(secs) => {
                if pref!(self.fetch_interval_secs = secs) {
                    self.arm_auto_fetch(&sender);
                }
            }

            PrefOutput::SetMeteredSaveData(on) => {
                if pref!(self.metered_save_data = on) {
                    let was = crate::desktop::power::saving_data();
                    crate::desktop::power::set_save_data(on);
                    self.saving_data_changed(was);
                }
            }

            PrefOutput::SetPush(on) => {
                if pref!(self.push = on) {
                    // Workers read the push setting at startup; restart to apply.
                    self.reconnect_all(&sender);
                }
            }

            PrefOutput::SetNotifications(on) => {
                pref!(self.notifications_enabled = on);
            }

            PrefOutput::SetNotificationContent(on) => {
                pref!(self.notification_content = on);
            }

            PrefOutput::SetNotifyReceipts(on) => {
                pref!(self.notify_receipts = on);
            }

            PrefOutput::SetNotificationButtons(buttons) => {
                pref!(self.notification_buttons = buttons);
            }

            PrefOutput::SetRunInBackground(on) => {
                if self.run_in_background.get() != on {
                    self.run_in_background.set(on);
                    self.save_settings();
                    // Ask the portal as the setting changes, so the permission
                    // dialog appears while the user is looking at the switch they
                    // just moved rather than at some later, unexplained moment.
                    crate::desktop::background::request(on && self.autostart);
                    if !on {
                        crate::desktop::background::set_status("");
                    }
                }
            }

            PrefOutput::SetAutostart(on) => {
                if pref!(self.autostart = on) {
                    crate::desktop::background::request(self.run_in_background.get() && on);
                }
            }

            PrefOutput::SetTray(on) => {
                if pref!(self.tray_enabled = on) {
                    if on {
                        self.start_tray(&sender);
                    } else if let Some(tray) = self.tray.take() {
                        tray.stop();
                    }
                }
            }

            PrefOutput::SetTrayIcon(icon) => {
                if pref!(self.tray_icon = icon) {
                    // A fresh item rather than a new icon on the old one: the
                    // AppIndicator extension, taken from a picture back to a
                    // symbolic icon's file, kept drawing the picture (#258).
                    // A new item is set up from nothing, as at startup.
                    if let Some(tray) = self.tray.take() {
                        tray.stop();
                        self.start_tray(&sender);
                    }
                }
            }

            PrefOutput::SetAppIcon(id) => {
                // Picking the stored choice again still changes the desktop
                // when it replaces an icon set outside Hylki (#252).
                let replaces_custom = crate::desktop::app_icon::custom_icon().is_some();
                let id = crate::desktop::app_icon::set(&id);
                if self.app_icon != id || replaces_custom {
                    self.app_icon = id;
                    if let Some(tray) = &self.tray {
                        tray.set_icon(
                            self.tray_icon,
                            &crate::desktop::app_icon::tray_image(&self.app_icon),
                        );
                    }
                    self.offer_restart_for_icon(&sender);
                }
            }

            PrefOutput::SetTrayMail(on) => {
                if pref!(self.tray_mail = on) {
                    // A sentinel no real list equals, so the change is sent
                    // whichever way the switch went.
                    *self.tray_mail_key.borrow_mut() =
                        Some(vec![(u32::MAX, u32::MAX, String::new(), String::new())]);
                    self.push_tray_mail();
                }
            }

            PrefOutput::SetLauncherBadge(badge) => {
                if pref!(self.launcher_badge = badge) {
                    self.push_launcher_count();
                }
            }

            PrefOutput::SetSingleKey(on) => {
                if self.single_key.get() != on {
                    self.single_key.set(on);
                    crate::ui::keymap::set_key_mode(on, self.gmail_keys.get());
                    self.save_settings();
                }
            }

            PrefOutput::SetStartFullscreen(on) => {
                pref!(self.start_fullscreen = on);
            }

            PrefOutput::SetKeepDownloads(days) => {
                if self.keep_downloads_days != days {
                    self.keep_downloads_days = days;
                    self.save_settings();
                    prune_downloads(days);
                }
            }

            PrefOutput::SetGmailKeys(on) => {
                if self.gmail_keys.get() != on {
                    self.gmail_keys.set(on);
                    crate::ui::keymap::set_key_mode(self.single_key.get(), on);
                    self.message_list.emit(MessageListInput::SetGmailKeys(on));
                    self.save_settings();
                }
            }
            PrefOutput::SetDesktopIcons(on) => {
                if pref!(self.desktop_icons = on) {
                    apply_icon_theme(on);
                }
            }

            PrefOutput::SetReadMark(policy) => {
                if pref!(self.read_mark = policy) {
                    self.message_view
                        .emit(MessageViewInput::SetReadMark(policy));
                }
            }

            PrefOutput::SetStatusBarTop(top) => {
                if pref!(self.status_bar_top = top) {
                    self.notifications.emit(NotifyInput::SetAtTop(top));
                }
            }

            PrefOutput::SetConsoleMode(on) => {
                if pref!(self.console_mode = on) {
                    self.notifications.emit(NotifyInput::SetConsoleEnabled(on));
                    self.rebuild_help_menu();
                }
            }

            PrefOutput::SetSettingsOpenAccounts(on) => {
                pref!(self.settings_open_accounts = on);
            }
            _ => unreachable!("not routed to on_prefs_general_pref"),
        }
    }
}
