//! Settings → Reading: the reading pane, Reader View and printing.

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, Preferences, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct ReadingPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) read_mark_row: adw::ComboRow,
    pub(in crate::ui::preferences) message_theme_row: adw::ComboRow,
    pub(in crate::ui::preferences) theme_switch_row: adw::SwitchRow,
    pub(in crate::ui::preferences) account_chip_row: adw::SwitchRow,
    pub(in crate::ui::preferences) reader_zoom_row: adw::ComboRow,
    pub(in crate::ui::preferences) override_fonts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) reader_font_row: adw::ActionRow,
    pub(in crate::ui::preferences) reader_font_button: gtk::FontDialogButton,
    pub(in crate::ui::preferences) override_colors_row: adw::SwitchRow,
    pub(in crate::ui::preferences) plain_mono_row: adw::SwitchRow,
    pub(in crate::ui::preferences) plain_font_row: adw::ActionRow,
    pub(in crate::ui::preferences) plain_font_button: gtk::FontDialogButton,
    pub(in crate::ui::preferences) card_actions_row: adw::ComboRow,
    pub(in crate::ui::preferences) card_palette_collapse_row: adw::SpinRow,
    pub(in crate::ui::preferences) card_palette_menu_row: adw::SwitchRow,
    pub(in crate::ui::preferences) single_message_card_row: adw::SwitchRow,
    pub(in crate::ui::preferences) always_show_recipients_row: adw::SwitchRow,
    pub(in crate::ui::preferences) pgp_labels_row: adw::SwitchRow,
    pub(in crate::ui::preferences) find_place_row: adw::ComboRow,
    pub(in crate::ui::preferences) reader_switch_row: adw::SwitchRow,
    pub(in crate::ui::preferences) reader_default_row: adw::ComboRow,
    pub(in crate::ui::preferences) print_page_numbers_row: adw::SwitchRow,
    pub(in crate::ui::preferences) print_date_row: adw::SwitchRow,
    pub(in crate::ui::preferences) print_punch_margin_row: adw::SwitchRow,
}

impl ReadingPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Reading"),

                    #[name = "read_mark_row"]
                    adw::ComboRow {
                        set_title: &i18n("Mark as read"),
                        set_subtitle: &i18n("Conversations mark each message as it comes into view."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeReadMark(row.selected()));
                        },
                    },

                    #[name = "message_theme_row"]
                    adw::ComboRow {
                        set_title: &i18n("Message appearance"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeMessageTheme(row.selected()));
                        },
                    },

                    #[name = "theme_switch_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Light / Dark Mode switch"),
                        set_subtitle: &i18n("Displays in the message header for each message."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetThemeSwitch(row.is_active()));
                        },
                    },

                    #[name = "account_chip_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show account in message header"),
                        set_subtitle: &i18n("The account a message is in, above its subject."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAccountChip(row.is_active()));
                        },
                    },

                    #[name = "reader_zoom_row"]
                    adw::ComboRow {
                        set_title: &i18n("Default zoom"),
                        set_subtitle: &i18n("Ctrl+0 returns to the default."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeReaderZoom(row.selected()));
                        },
                    },

                    #[name = "override_fonts_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Use my own font"),
                        set_subtitle: &i18n("The font below replaces the sender's."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetOverrideFonts(row.is_active()));
                        },
                    },

                    #[name = "reader_font_row"]
                    adw::ActionRow {
                        set_title: &i18n("Message font"),
                        set_subtitle: &i18n("The interface font unless another is chosen."),
                        #[name = "reader_font_button"]
                        add_suffix = &gtk::FontDialogButton {
                            set_valign: gtk::Align::Center,
                            set_dialog: &gtk::FontDialog::new(),
                            set_level: gtk::FontLevel::Font,
                            set_use_font: true,
                            connect_font_desc_notify[sender] => move |button| {
                                let font = button
                                    .font_desc()
                                    .map(|d| d.to_string())
                                    .unwrap_or_default();
                                let _ = sender.output(PrefOutput::SetReaderFont(font));
                            },
                        },
                    },

                    #[name = "override_colors_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Use my own colors"),
                        set_subtitle: &i18n("Ignores the sender's colors. Pictures stay; links use the accent color."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetOverrideColors(row.is_active()));
                        },
                    },

                    #[name = "plain_mono_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Plain-text messages in monospace"),
                        set_subtitle: &i18n("Formatted messages are not affected."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetPlainMonospace(row.is_active()));
                        },
                    },

                    #[name = "plain_font_row"]
                    adw::ActionRow {
                        set_title: &i18n("Monospace font"),
                        set_subtitle: &i18n("The system's monospace font unless another is chosen."),
                        #[name = "plain_font_button"]
                        add_suffix = &gtk::FontDialogButton {
                            set_valign: gtk::Align::Center,
                            set_dialog: &gtk::FontDialog::new(),
                            set_level: gtk::FontLevel::Font,
                            set_use_font: true,
                            connect_font_desc_notify[sender] => move |button| {
                                let font = button
                                    .font_desc()
                                    .map(|d| d.to_string())
                                    .unwrap_or_default();
                                let _ = sender.output(PrefOutput::SetPlainFont(font));
                            },
                        },
                    },

                    #[name = "card_actions_row"]
                    adw::ComboRow {
                        set_title: &i18n("Message card actions palette"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeCardActionsMode(row.selected()));
                        },
                    },

                    #[name = "card_palette_collapse_row"]
                    adw::SpinRow {
                        set_title: &i18n("Message card actions palette timeout"),
                        set_subtitle: &i18n("Seconds it stays open after the pointer leaves."),
                        connect_value_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetCardPaletteCollapse(row.value() as u64));
                        },
                    },

                    // Only the hidden-behind-a-toggle mode has
                    // a ⋯ to press, so the choice is only live
                    // there.
                    #[name = "card_palette_menu_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.card_actions_hover,
                        set_title: &i18n("Message card actions palette as a menu"),
                        set_subtitle: &i18n("The ⋯ opens the right-click menu instead."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetCardPaletteMenu(row.is_active()));
                        },
                    },

                    #[name = "single_message_card_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Single messages as cards"),
                        set_subtitle: &i18n("Off fills the pane edge to edge."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSingleMessageCard(row.is_active()));
                        },
                    },

                    #[name = "always_show_recipients_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Always show recipients"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAlwaysShowRecipients(row.is_active()));
                        },
                    },

                    #[name = "pgp_labels_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Name the OpenPGP result"),
                        set_subtitle: &i18n("Off shows the icons alone."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetPgpLabels(row.is_active()));
                        },
                    },

                    #[name = "find_place_row"]
                    adw::ComboRow {
                        set_title: &i18n("Find bar position"),
                        connect_selected_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetReaderFindBottom(row.selected() == 1));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Reader View"),
                    set_description: Some(&i18n("A message's text alone, without the sender's layout, colors and fonts.")),

                    #[name = "reader_switch_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show Reader View switch in message header"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetReaderSwitch(row.is_active()));
                        },
                    },

                    #[name = "reader_default_row"]
                    adw::ComboRow {
                        set_title: &i18n("When a message is opened"),
                        set_subtitle: &i18n("The header's switch still changes the open message."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeReaderDefault(row.selected()));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Printing"),

                    #[name = "print_page_numbers_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Page numbers"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::TogglePrintPageNumbers(row.is_active()));
                        },
                    },

                    #[name = "print_date_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Date printed"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::TogglePrintDate(row.is_active()));
                        },
                    },

                    #[name = "print_punch_margin_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Wider left margin"),
                        set_subtitle: &i18n("25 mm, so punched holes miss the text."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::TogglePrintPunchMargin(row.is_active()));
                        },
                    },
                },
            }
        }
        Self {
            page,
            read_mark_row,
            message_theme_row,
            theme_switch_row,
            account_chip_row,
            reader_zoom_row,
            override_fonts_row,
            reader_font_row,
            reader_font_button,
            override_colors_row,
            plain_mono_row,
            plain_font_row,
            plain_font_button,
            card_actions_row,
            card_palette_collapse_row,
            card_palette_menu_row,
            single_message_card_row,
            always_show_recipients_row,
            pgp_labels_row,
            find_place_row,
            reader_switch_row,
            reader_default_row,
            print_page_numbers_row,
            print_date_row,
            print_punch_margin_row,
        }
    }

    /// The view's `#[watch]` setters for this page, in the order the
    /// view had them (see the module comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.card_palette_menu_row
            .set_sensitive(model.card_actions_hover);
    }
}
