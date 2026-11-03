//! Applying message list settings.

use relm4::prelude::*;

use crate::config;
use crate::ui::contacts_page::ContactsPageInput;
use crate::ui::message_list::MessageListInput;
use crate::ui::preferences::PrefOutput;
use crate::ui::sidebar::SidebarInput;

use super::model::AppModel;
use super::msg::AppMsg;

impl AppModel {
    /// The settings changes the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_list_pref(&mut self, pref: PrefOutput, sender: ComponentSender<Self>) {
        match pref {
            PrefOutput::SetListLayout(layout) => {
                if pref!(self.list_layout = layout) {
                    self.push_single_line();
                }
            }
            PrefOutput::SetListHeadings(on) => {
                if pref!(self.list_headings = on) {
                    self.message_list.emit(MessageListInput::SetHeadings(on));
                }
            }
            PrefOutput::SetListColumnScroll(on) => {
                if pref!(self.list_column_scroll = on) {
                    self.message_list
                        .emit(MessageListInput::SetColumnScroll(on));
                }
            }
            PrefOutput::SetListSearchOpen(on) => {
                if pref!(self.list_search_open = on) {
                    self.message_list
                        .emit(MessageListInput::SetSearchPinned(on));
                }
            }
            PrefOutput::SetContactNames(on) => {
                if pref!(self.contact_names = on) {
                    self.read_contact_names(&sender);
                }
            }
            PrefOutput::SetContactsSearchOpen(on) => {
                if pref!(self.contacts_search_open = on) {
                    self.contacts_page
                        .emit(ContactsPageInput::SetSearchPinned(on));
                }
            }
            PrefOutput::SetListSearchBottom(on) => {
                if pref!(self.list_search_bottom = on) {
                    self.push_list_bars();
                }
            }
            PrefOutput::SetListBulkBarBottom(on) => {
                if pref!(self.list_bulk_bar_bottom = on) {
                    self.push_list_bars();
                }
            }
            PrefOutput::SetGroupByDate(on) => {
                if pref!(self.list_group_by_date = on) {
                    self.message_list.emit(MessageListInput::SetGroupByDate(on));
                }
            }
            PrefOutput::SetPillsBelow(on) => {
                if pref!(self.list_pills_below = on) {
                    self.message_list.emit(MessageListInput::SetPillsBelow(on));
                }
            }
            PrefOutput::SetListColumns(columns) => {
                if pref!(self.list_columns = columns.clone()) {
                    self.message_list
                        .emit(MessageListInput::SetColumns(columns));
                }
            }
            PrefOutput::SetPreviewLines(lines) => {
                if self.preview_lines != lines {
                    let was_off = self.preview_lines == 0;
                    self.preview_lines = lines;
                    self.save_settings();
                    self.push_list_look(false);
                    // Previews switched back on: IMAP summaries fetched while
                    // they were off carry no preview text (the setting also
                    // stops the body slice being downloaded), so nothing would
                    // show until the next scheduled sync. Refresh now so they
                    // fill in right away. (Graph always has bodyPreview in
                    // hand, which is why Microsoft accounts showed instantly.)
                    if was_off && lines > 0 {
                        sender.input(AppMsg::Refresh);
                    }
                }
            }

            PrefOutput::SetAttachmentsRow(show) => {
                if pref!(self.show_attachments = show) {
                    self.sidebars_emit(SidebarInput::SetAttachmentsRow(show));
                }
            }
            PrefOutput::ResetColumnWidths => {
                if pref!(self.list_column_widths = Default::default()) {
                    self.message_list
                        .emit(MessageListInput::SetColumnWidths(Default::default()));
                }
            }

            PrefOutput::SetPaletteCollapse(secs) => {
                if pref!(self.palette_collapse_secs = secs) {
                    self.message_list
                        .emit(MessageListInput::SetPaletteCollapse(secs));
                }
            }

            PrefOutput::SetListPalette(on) => {
                if pref!(self.list_palette = on) {
                    self.message_list.emit(MessageListInput::SetListPalette(on));
                }
            }

            PrefOutput::SetListPaletteHover(on) => {
                if pref!(self.list_palette_hover = on) {
                    self.message_list
                        .emit(MessageListInput::SetPaletteHover(on));
                }
            }

            PrefOutput::SetSwipeEnabled(on) => {
                if pref!(self.swipe_enabled = on) {
                    self.message_list
                        .emit(MessageListInput::SetSwipeEnabled(on));
                }
            }

            PrefOutput::SetSwipeReversed(on) => {
                if pref!(self.swipe_reversed = on) {
                    self.message_list
                        .emit(MessageListInput::SetSwipeReversed(on));
                }
            }

            PrefOutput::SetSwipeSensitivity(factor) => {
                let factor =
                    factor.clamp(config::SWIPE_SENSITIVITY_MIN, config::SWIPE_SENSITIVITY_MAX);
                if pref!(self.swipe_sensitivity = factor) {
                    self.message_list
                        .emit(MessageListInput::SetSwipeSensitivity(factor));
                }
            }
            _ => unreachable!("not routed to on_prefs_list_pref"),
        }
    }
}
