//! Applying reader settings: zoom, Reader View, font and color overrides.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::ui::preferences::PrefOutput;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;

impl AppModel {
    /// The session's message zoom: every reader on screen follows.
    pub(super) fn set_zoom(&mut self, zoom: u32) {
        if zoom == self.zoom {
            return;
        }
        self.zoom = zoom;
        self.message_view.emit(MessageViewInput::SetZoom(zoom));
        self.popouts_emit(|| MessageWindowInput::SetZoom(zoom));
    }

    /// What Reader View does on each open: Focus Mode's "on" while it holds,
    /// else the setting.
    pub(super) fn effective_reader_default(&self) -> config::ReaderDefault {
        if self.focus.active(config::FocusPart::ReaderView) {
            config::ReaderDefault::On
        } else {
            self.reader_default
        }
    }

    /// Reader View for the message on screen: on in Focus Mode, else as saved.
    pub(super) fn effective_reader_mode(&self) -> bool {
        self.reader_mode || self.focus.active(config::FocusPart::ReaderView)
    }

    /// The reader's font-and-color override (#56) as the reader applies it:
    /// the font only when the switch is on, the interface font standing in
    /// where none was chosen.
    pub(super) fn reader_style(&self) -> config::ReaderStyle {
        let font = self.override_fonts.then(|| {
            if self.reader_font.trim().is_empty() {
                interface_font()
            } else {
                self.reader_font.clone()
            }
        });
        let plain_font = self.plain_monospace.then(|| {
            if self.plain_font.trim().is_empty() {
                crate::desktop::session::monospace_font()
            } else {
                self.plain_font.clone()
            }
        });
        config::ReaderStyle {
            font,
            colors: self.override_colors,
            plain_font,
        }
    }

    /// Hand the current reader style to the reader and every popped-out window.
    fn push_reader_style(&self) {
        let style = self.reader_style();
        self.message_view
            .emit(MessageViewInput::SetReaderStyle(style.clone()));
        self.popouts_emit(|| MessageWindowInput::SetReaderStyle(style.clone()));
    }

    /// Tell the reader how card actions should show (⋯ toggle / auto on
    /// hover / always).
    fn push_card_actions_mode(&self) {
        self.message_view
            .emit(MessageViewInput::SetCardActionsMode {
                hover_toggle: self.card_actions_hover,
                hover_auto: self.card_actions_auto,
            });
    }

    /// The settings changes the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_reader_pref(&mut self, pref: PrefOutput) {
        match pref {
            PrefOutput::SetReaderToolbar(layout) => {
                if self.reader_toolbar != layout {
                    self.reader_toolbar = layout;
                    config::save_reader_toolbar(&self.reader_toolbar);
                    self.relayout_reader_toolbar();
                    self.popouts_emit(|| {
                        MessageWindowInput::SetToolbar(self.reader_toolbar.clone())
                    });
                    // Not while the inline composer covers the header: that
                    // path hides the ⋯ by hand and restores it on close.
                    if !self
                        .reader_compose
                        .as_ref()
                        .is_some_and(|r| r.window.is_none())
                    {
                        self.reader_overflow_btn
                            .set_visible(self.reader_overflow_wanted());
                    }
                }
            }

            PrefOutput::SetToolbarFoldRight(on) => {
                if pref!(self.toolbar_fold_right = on) {
                    // Shown again, the right group needs the fold point
                    // measured afresh.
                    self.refresh_reader_breakpoint();
                    self.popouts_emit(|| MessageWindowInput::SetToolbarFold(on));
                    if !self
                        .reader_compose
                        .as_ref()
                        .is_some_and(|r| r.window.is_none())
                    {
                        self.reader_overflow_btn
                            .set_visible(self.reader_overflow_wanted());
                    }
                }
            }

            PrefOutput::SetShowRemoteBanner(on) => {
                if pref!(self.show_remote_banner = on) {
                    self.message_view.emit(MessageViewInput::SetBannerShown(on));
                }
            }

            PrefOutput::SetReceipts(policy) => {
                if pref!(self.receipts = policy) {
                    self.refresh_receipt_banners();
                }
            }

            PrefOutput::SetShowSpoofBanner(on) => {
                if pref!(self.show_spoof_banner = on) {
                    self.message_view
                        .emit(MessageViewInput::SetSpoofBannerShown(on));
                    self.popouts_emit(|| MessageWindowInput::SetSpoofBannerShown(on));
                }
            }

            PrefOutput::SetRemoteAhead(on) => {
                if pref!(self.remote_ahead = on) && !on {
                    crate::services::remote_content::fetch_ahead([]);
                }
            }

            PrefOutput::SetAutoRemoteContent(on) => {
                if pref!(self.auto_remote_content = on) {
                    // Re-render what is open so the change takes effect there too:
                    // on, the blocked content loads; off, it is stripped again.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    } else {
                        let current = self.current.clone();
                        self.show_message(current, false);
                    }
                }
            }

            PrefOutput::SetThreading(on) => {
                if pref!(self.threading = on) {
                    self.message_list.emit(MessageListInput::SetThreading(on));
                }
            }

            PrefOutput::SetThreadExpansion(on) => {
                if pref!(self.thread_expansion = on) {
                    self.message_list
                        .emit(MessageListInput::SetThreadExpansion(on));
                }
            }

            PrefOutput::SetThreadRowNewest(on) => {
                if pref!(self.thread_row_newest = on) {
                    self.message_list
                        .emit(MessageListInput::SetThreadRowNewest(on));
                }
            }

            PrefOutput::SetConfirmThreadDelete(on) => {
                pref!(self.confirm_thread_delete = on);
            }

            PrefOutput::SetCardActionsMode {
                hover_toggle,
                hover_auto,
            } => {
                if self.card_actions_hover != hover_toggle || self.card_actions_auto != hover_auto {
                    self.card_actions_hover = hover_toggle;
                    self.card_actions_auto = hover_auto;
                    self.save_settings();
                    self.push_card_actions_mode();
                }
            }

            PrefOutput::SetThreadsExpanded(on) => {
                if pref!(self.threads_expanded = on) {
                    self.message_list
                        .emit(MessageListInput::SetThreadsExpanded(on));
                }
            }

            PrefOutput::SetThreadNewestFirst(on) => {
                if pref!(self.thread_newest_first = on) {
                    // Re-render an open conversation in the new order.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    }
                }
            }

            PrefOutput::SetFoldMessages(fold) => {
                if pref!(self.fold_messages = fold) {
                    self.message_view
                        .emit(MessageViewInput::SetFoldMessages(fold));
                }
            }

            PrefOutput::SetAlwaysShowRecipients(on) => {
                if pref!(self.always_show_recipients = on) {
                    self.message_view
                        .emit(MessageViewInput::SetAlwaysShowRecipients(on));
                    // Re-render whatever is open so the header reflects it.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    } else if let Some(current) = self.current.clone() {
                        let m = self.with_cached_body(current);
                        self.show_message(Some(m), false);
                    }
                }
            }

            PrefOutput::SetCardAttachments(on) => {
                if pref!(self.card_attachments = on) {
                    self.message_view
                        .emit(MessageViewInput::SetCardAttachmentsShown(on));
                    self.popouts_emit(|| MessageWindowInput::SetCardAttachmentsShown(on));
                }
            }
            PrefOutput::SetAttachmentDrawer(on) => {
                if pref!(self.drawer_enabled = on) {
                    self.sync_attachment_drawer();
                    self.message_view
                        .emit(MessageViewInput::SetAttachmentDrawer(on));
                    self.popouts_emit(|| MessageWindowInput::SetAttachmentDrawer(on));
                }
            }
            PrefOutput::SetReaderDefault(policy) => {
                if pref!(self.reader_default = policy) {
                    // Focus Mode's Reader View outranks it while on.
                    let policy = self.effective_reader_default();
                    self.message_view
                        .emit(MessageViewInput::SetReaderDefault(policy));
                    self.popouts_emit(|| MessageWindowInput::SetReaderDefault(policy));
                }
            }
            PrefOutput::SetPgpLabels(on) => {
                if pref!(self.pgp_labels = on) {
                    self.message_view.emit(MessageViewInput::SetPgpLabels(on));
                    self.popouts_emit(|| MessageWindowInput::SetPgpLabels(on));
                }
            }
            PrefOutput::SetAccountChip(on) => {
                if pref!(self.account_chip = on) {
                    self.message_view
                        .emit(MessageViewInput::SetAccountChipShown(on));
                    self.popouts_emit(|| MessageWindowInput::SetAccountChipShown(on));
                }
            }
            PrefOutput::SetReaderFindBottom(on) => {
                if pref!(self.reader_find_bottom = on) {
                    self.message_view.emit(MessageViewInput::SetFindBottom(on));
                    self.popouts_emit(|| MessageWindowInput::SetFindBottom(on));
                }
            }
            PrefOutput::SetSingleMessageCard(on) => {
                if pref!(self.single_message_card = on) {
                    self.message_view
                        .emit(MessageViewInput::SetSingleMessageCard(on));
                    // Only lone messages change; re-render one if it's open.
                    if self.current_thread.len() <= 1 {
                        if let Some(current) = self.current.clone() {
                            let m = self.with_cached_body(current);
                            self.show_message(Some(m), false);
                        }
                    }
                }
            }

            PrefOutput::SetCardPaletteCollapse(secs) => {
                if pref!(self.card_palette_collapse_secs = secs) {
                    self.message_view
                        .emit(MessageViewInput::SetPaletteCollapse(secs));
                }
            }

            PrefOutput::SetCardPaletteMenu(on) => {
                if pref!(self.card_palette_menu = on) {
                    self.message_view
                        .emit(MessageViewInput::SetCardPaletteMenu(on));
                }
            }

            PrefOutput::SetMessageTheme(theme) => {
                if pref!(self.message_theme = theme) {
                    let dark = theme.dark_override();
                    // Message content only — the reader and any popped-out windows.
                    self.message_view
                        .emit(MessageViewInput::SetContentTheme(dark));
                    self.popouts_emit(|| MessageWindowInput::SetContentTheme(dark));
                }
            }
            PrefOutput::SetOverrideFonts(on) => {
                if pref!(self.override_fonts = on) {
                    self.push_reader_style();
                }
            }
            PrefOutput::SetReaderFont(font) => {
                if pref!(self.reader_font = font) {
                    self.push_reader_style();
                }
            }
            PrefOutput::SetPlainMonospace(on) => {
                if pref!(self.plain_monospace = on) {
                    self.push_reader_style();
                }
            }
            PrefOutput::SetPlainFont(font) => {
                if pref!(self.plain_font = font) {
                    self.push_reader_style();
                }
            }
            PrefOutput::SetOverrideColors(on) => {
                if pref!(self.override_colors = on) {
                    self.push_reader_style();
                }
            }
            _ => unreachable!("not routed to on_prefs_reader_pref"),
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_reader(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::SetZoomDefault(zoom) => {
                if pref!(self.zoom_default = zoom) {
                    self.message_view
                        .emit(MessageViewInput::SetZoomDefault(zoom));
                    self.popouts_emit(|| MessageWindowInput::SetZoomDefault(zoom));
                }
                // The setting is also what the user wants to see now.
                self.set_zoom(zoom);
            }
            AppMsg::SetReaderMode(on) => {
                // In Focus Mode with Reader View on, the switch changes the
                // message on screen alone: the saved choice is the ordinary
                // one, and it comes back with the rest of the layout.
                if self.focus.active(config::FocusPart::ReaderView) {
                    // Nothing to remember.
                } else if self.reader_mode != on {
                    self.reader_mode = on;
                    self.save_settings();
                }
                // Always pushed, equal or not: with a per-message default the
                // readers reset themselves on each open, so what they show
                // can differ from the saved state — and a switch flipped in
                // one of them must land there regardless.
                self.message_view.emit(MessageViewInput::SetReaderMode(on));
                self.popouts_emit(|| MessageWindowInput::SetReaderMode(on));
            }
            AppMsg::SetReaderSwitchShown(on) => {
                if pref!(self.reader_switch = on) {
                    self.message_view
                        .emit(MessageViewInput::SetReaderSwitchShown(on));
                    self.popouts_emit(|| MessageWindowInput::SetReaderSwitchShown(on));
                }
            }
            AppMsg::SetThemeSwitchShown(on) => {
                if pref!(self.theme_switch = on) {
                    self.message_view
                        .emit(MessageViewInput::SetThemeSwitchShown(on));
                    self.popouts_emit(|| MessageWindowInput::SetThemeSwitchShown(on));
                }
            }

            AppMsg::SetRemoteContent {
                account_id,
                id,
                show,
            } => {
                self.remote_override.insert((account_id, id), show);
                // Re-show whatever is open so the decision takes effect now.
                // The reader re-reads `remote_allowed` for every message it
                // paints, so a conversation gets it for the card it applies to.
                if self.current_thread.len() > 1 {
                    self.show_thread();
                } else {
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
            }
            AppMsg::ReloadBody(m) => {
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(
                        m.account_id,
                        MailRequest::LoadBody {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                        },
                    );
                }
            }
            _ => unreachable!("not routed to on_prefs_reader"),
        }
    }
}

/// The desktop's interface font ("Adwaita Sans 11") as a Pango description:
/// what the reader sets mail in when the font override is on but no font
/// was chosen (#56). Cantarell 11 when GTK has no settings (headless).
fn interface_font() -> String {
    gtk::Settings::default()
        .and_then(|s| s.gtk_font_name())
        .map(|f| f.to_string())
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "Cantarell 11".to_string())
}
