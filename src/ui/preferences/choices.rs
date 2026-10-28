//! The choices the settings rows offer, in the order the rows list them,
//! and the conversions between a stored value and a row's index.

use super::*;

/// App-chrome appearance options, in combo order.
pub(super) const TRAY_ICONS: &[(&str, TrayIcon)] = &[
    (i18n_noop("App icon"), TrayIcon::Hylki),
    (i18n_noop("Symbolic"), TrayIcon::Symbolic),
];

/// The launcher badge's choices (#431), in the order the row lists them.
pub(super) const LAUNCHER_BADGES: &[(&str, LauncherBadge)] = &[
    (i18n_noop("Off"), LauncherBadge::Off),
    (i18n_noop("All unread inbox mail"), LauncherBadge::AllUnread),
    (
        i18n_noop("New since the window was last in front"),
        LauncherBadge::NewSinceFront,
    ),
];

pub(super) const APP_THEMES: &[(&str, AppTheme)] = &[
    (i18n_noop("Follow system"), AppTheme::System),
    (i18n_noop("Light"), AppTheme::Light),
    (i18n_noop("Dark"), AppTheme::Dark),
];

/// Message-content appearance options, in combo order.
pub(super) const MESSAGE_THEMES: &[(&str, MessageTheme)] = &[
    (i18n_noop("Follow system"), MessageTheme::System),
    (i18n_noop("Light"), MessageTheme::Light),
    (i18n_noop("Dark"), MessageTheme::Dark),
];

/// Date arrangements, in combo order. The examples are what each writes.
pub(super) const DATE_STYLES: &[(&str, DateStyle)] = &[
    (i18n_noop("Follow system"), DateStyle::System),
    (i18n_noop("Aug 23, 2026"), DateStyle::MonthFirst),
    (i18n_noop("23 Aug 2026"), DateStyle::DayFirst),
    (i18n_noop("2026 Aug 23"), DateStyle::YearFirst),
];

/// The Language row's choices: (label, locale code). The system's own
/// first, then English (the source language), then every catalogue in
/// po/LINGUAS by its own name (#179).
pub(crate) fn language_choices() -> Vec<(String, String)> {
    let mut out = vec![
        (i18n("System"), String::new()),
        ("English".to_string(), "en".to_string()),
    ];
    for code in include_str!("../../../po/LINGUAS").lines() {
        let code = code.trim();
        if code.is_empty() || code.starts_with('#') || code == "en" {
            continue;
        }
        out.push((native_language_name(code).to_string(), code.to_string()));
    }
    out
}

/// A language's name in itself, for the Language row.
fn native_language_name(code: &str) -> &str {
    match code {
        "ca" => "Català",
        "fr" => "Français",
        "el" => "Ελληνικά",
        "hu" => "Magyar",
        "ru" => "Русский",
        "de" => "Deutsch",
        "es" => "Español",
        "it" => "Italiano",
        "pt" => "Português",
        "pt_PT" => "Português (Portugal)",
        "pt_BR" => "Português (Brasil)",
        "nl" => "Nederlands",
        "pl" => "Polski",
        "cs" => "Čeština",
        "sv" => "Svenska",
        "da" => "Dansk",
        "nb" | "no" => "Norsk",
        "fi" => "Suomi",
        "tr" => "Türkçe",
        "uk" => "Українська",
        "ja" => "日本語",
        "zh_CN" => "简体中文",
        "zh_TW" => "繁體中文",
        "ko" => "한국어",
        other => other,
    }
}

/// Clock options, in combo order.
pub(super) const CLOCK_STYLES: &[(&str, ClockStyle)] = &[
    (i18n_noop("Follow system"), ClockStyle::System),
    (i18n_noop("12-hour (5:40 PM)"), ClockStyle::Twelve),
    (i18n_noop("24-hour (17:40)"), ClockStyle::TwentyFour),
];

/// Selectable mail-check intervals (label, seconds). 0 = manual only.
pub(super) const FETCH_INTERVALS: &[(&str, u64)] = &[
    (i18n_noop("Manually"), 0),
    (i18n_noop("Every minute"), 60),
    (i18n_noop("Every 5 minutes"), 300),
    (i18n_noop("Every 15 minutes"), 900),
    (i18n_noop("Every 30 minutes"), 1800),
];

/// The GNOME Files combo rows' positions for each preference value.
pub(super) fn files_action_index(a: crate::config::FilesAction) -> u32 {
    use crate::config::FilesAction as A;
    match a {
        A::Ask => 0,
        A::New => 1,
        A::Draft => 2,
        A::Reply => 3,
    }
}

/// The links combo's position for a stored choice: 0 the desktop's default,
/// 1 the chooser, otherwise the browser's place in the list. A browser that
/// is no longer installed (or cannot be seen from inside the sandbox) reads
/// as the default, which is what its links will do.
pub(super) fn link_browser_index(browsers: &[crate::ui::launch::Browser], choice: &str) -> u32 {
    if choice.is_empty() {
        return 0;
    }
    if choice == crate::ui::launch::ASK {
        return 1;
    }
    browsers
        .iter()
        .position(|b| b.id == choice)
        .map(|i| i as u32 + 2)
        .unwrap_or(0)
}

pub(super) fn files_large_index(l: crate::config::FilesLarge) -> u32 {
    use crate::config::FilesLarge as L;
    match l {
        L::Ask => 0,
        L::Attach => 1,
        L::Cloud => 2,
    }
}

/// The choices of Settings → System → Renderer, as `GSK_RENDERER` values;
/// empty is GTK's own choice (#403).
pub(super) const RENDERERS: [&str; 4] = ["", "vulkan", "gl", "cairo"];

/// The choices of Settings → System → Keep downloaded mail, in days; 0 keeps
/// it (#394).
pub(super) const KEEP_DOWNLOADS_DAYS: [u32; 4] = [0, 365, 90, 30];

/// The placement combos' rows. The unified section is not one: loading
/// moves it above the accounts.
pub(super) fn placement_index(p: crate::config::SectionPlacement) -> u32 {
    use crate::config::SectionPlacement::*;
    match p {
        AllInboxes | AboveAccounts | Footer => 0,
        BelowAccounts => 1,
        Off => 2,
    }
}

pub(super) fn placement_from_index(idx: u32) -> crate::config::SectionPlacement {
    use crate::config::SectionPlacement::*;
    match idx {
        1 => BelowAccounts,
        2 => Off,
        _ => AboveAccounts,
    }
}

/// The Contacts placement combos' choices, here and in Settings →
/// Contacts: Contacts alone can sit in the footer, first as the default.
pub(crate) fn contacts_placement_model() -> gtk::StringList {
    gtk::StringList::new(&[
        i18n("In the footer").as_str(),
        i18n("Above the accounts").as_str(),
        i18n("Below the accounts").as_str(),
        i18n("Off").as_str(),
    ])
}

/// The Contacts placement combo's rows: the footer, then the others.
pub(crate) fn contacts_placement_index(p: crate::config::SectionPlacement) -> u32 {
    match p {
        crate::config::SectionPlacement::Footer => 0,
        p => placement_index(p) + 1,
    }
}

pub(crate) fn contacts_placement_from_index(idx: u32) -> crate::config::SectionPlacement {
    match idx {
        0 => crate::config::SectionPlacement::Footer,
        idx => placement_from_index(idx - 1),
    }
}

/// The new-mail sound choices, in `BUILTIN_SOUNDS` order, then Custom File.
pub(super) fn sound_labels() -> Vec<String> {
    // GNOME Settings' own names for its alert sounds.
    let mut labels = vec![i18n("Click"), i18n("Hum"), i18n("String"), i18n("Swing")];
    labels.push(i18n("Custom File"));
    labels
}

/// The row of `sound_labels` naming a saved `sound` value.
pub(super) fn sound_index(sound: &str) -> u32 {
    crate::config::BUILTIN_SOUNDS
        .iter()
        .position(|n| *n == sound)
        .unwrap_or(if sound == crate::config::CUSTOM_SOUND {
            crate::config::BUILTIN_SOUNDS.len()
        } else {
            0
        }) as u32
}

/// The row of the start view combo that shows `value`.
pub(super) fn start_view_index(value: crate::config::StartView) -> u32 {
    use crate::config::StartView;
    match value {
        StartView::AllInboxes => 0,
        StartView::AccountInbox => 1,
        StartView::LastFolder => 2,
    }
}

/// The start view value at row `idx` of its combo; past the end reads as the first.
pub(super) fn start_view_at(idx: u32) -> crate::config::StartView {
    use crate::config::StartView;
    match idx {
        1 => StartView::AccountInbox,
        2 => StartView::LastFolder,
        _ => StartView::AllInboxes,
    }
}

/// The row of the list layout combo that shows `value`.
pub(super) fn list_layout_index(value: crate::config::ListLayout) -> u32 {
    use crate::config::ListLayout;
    match value {
        ListLayout::Cards => 0,
        ListLayout::SingleLine => 1,
        ListLayout::Automatic => 2,
    }
}

/// The list layout value at row `idx` of its combo; past the end reads as the first.
pub(super) fn list_layout_at(idx: u32) -> crate::config::ListLayout {
    use crate::config::ListLayout;
    match idx {
        1 => ListLayout::SingleLine,
        2 => ListLayout::Automatic,
        _ => ListLayout::Cards,
    }
}

/// The row of the read mark combo that shows `value`.
pub(super) fn read_mark_index(value: crate::config::ReadMark) -> u32 {
    use crate::config::ReadMark;
    match value {
        ReadMark::Shown => 0,
        ReadMark::Delay => 1,
        ReadMark::Manual => 2,
    }
}

/// The read mark value at row `idx` of its combo; past the end reads as the first.
pub(super) fn read_mark_at(idx: u32) -> crate::config::ReadMark {
    use crate::config::ReadMark;
    match idx {
        1 => ReadMark::Delay,
        2 => ReadMark::Manual,
        _ => ReadMark::Shown,
    }
}

/// The row of the read receipts combo that shows `value` (#520).
pub(super) fn receipts_index(value: crate::config::ReceiptPolicy) -> u32 {
    use crate::config::ReceiptPolicy;
    match value {
        ReceiptPolicy::Ask => 0,
        ReceiptPolicy::Always => 1,
        ReceiptPolicy::Never => 2,
    }
}

/// The read receipts value at row `idx` of its combo; past the end asks.
pub(super) fn receipts_at(idx: u32) -> crate::config::ReceiptPolicy {
    use crate::config::ReceiptPolicy;
    match idx {
        1 => ReceiptPolicy::Always,
        2 => ReceiptPolicy::Never,
        _ => ReceiptPolicy::Ask,
    }
}

/// The row of the returned receipts combo that shows `value` (#520).
pub(super) fn returned_receipts_index(value: crate::config::ReturnedReceipts) -> u32 {
    use crate::config::ReturnedReceipts;
    match value {
        ReturnedReceipts::Trash => 0,
        ReturnedReceipts::Archive => 1,
        ReturnedReceipts::Keep => 2,
    }
}

/// The returned receipts value at row `idx` of its combo; past the end
/// moves to Trash.
pub(super) fn returned_receipts_at(idx: u32) -> crate::config::ReturnedReceipts {
    use crate::config::ReturnedReceipts;
    match idx {
        1 => ReturnedReceipts::Archive,
        2 => ReturnedReceipts::Keep,
        _ => ReturnedReceipts::Trash,
    }
}

/// The row of the fold messages combo that shows `value`.
pub(super) fn fold_messages_index(value: crate::config::FoldMessages) -> u32 {
    use crate::config::FoldMessages;
    match value {
        FoldMessages::Never => 0,
        FoldMessages::Read => 1,
        FoldMessages::AllButNewest => 2,
    }
}

/// The fold messages value at row `idx` of its combo; past the end reads as the first.
pub(super) fn fold_messages_at(idx: u32) -> crate::config::FoldMessages {
    use crate::config::FoldMessages;
    match idx {
        1 => FoldMessages::Read,
        2 => FoldMessages::AllButNewest,
        _ => FoldMessages::Never,
    }
}

/// The row of the reader default combo that shows `value`.
pub(super) fn reader_default_index(value: crate::config::ReaderDefault) -> u32 {
    use crate::config::ReaderDefault;
    match value {
        ReaderDefault::Remember => 0,
        ReaderDefault::On => 1,
        ReaderDefault::Off => 2,
    }
}

/// The reader default value at row `idx` of its combo; past the end reads as the first.
pub(super) fn reader_default_at(idx: u32) -> crate::config::ReaderDefault {
    use crate::config::ReaderDefault;
    match idx {
        1 => ReaderDefault::On,
        2 => ReaderDefault::Off,
        _ => ReaderDefault::Remember,
    }
}

/// The row of the compose format combo that shows `value`.
pub(super) fn compose_format_index(value: crate::config::ComposeFormat) -> u32 {
    use crate::config::ComposeFormat;
    match value {
        ComposeFormat::Rich => 0,
        ComposeFormat::Markdown => 1,
        ComposeFormat::Html => 2,
        ComposeFormat::Plain => 3,
    }
}

/// The compose format value at row `idx` of its combo; past the end reads as the first.
pub(super) fn compose_format_at(idx: u32) -> crate::config::ComposeFormat {
    use crate::config::ComposeFormat;
    match idx {
        1 => ComposeFormat::Markdown,
        2 => ComposeFormat::Html,
        3 => ComposeFormat::Plain,
        _ => ComposeFormat::Rich,
    }
}

/// The row of the signature position combo that shows `value`.
pub(super) fn signature_position_index(value: crate::config::SignaturePosition) -> u32 {
    use crate::config::SignaturePosition;
    match value {
        SignaturePosition::AboveQuote => 0,
        SignaturePosition::BelowQuote => 1,
    }
}

/// The signature position value at row `idx` of its combo; past the end reads as the first.
pub(super) fn signature_position_at(idx: u32) -> crate::config::SignaturePosition {
    use crate::config::SignaturePosition;
    match idx {
        1 => SignaturePosition::BelowQuote,
        _ => SignaturePosition::AboveQuote,
    }
}

/// The row of the reply position combo that shows `value`.
pub(super) fn reply_position_index(value: crate::config::ReplyPosition) -> u32 {
    use crate::config::ReplyPosition;
    match value {
        ReplyPosition::Top => 0,
        ReplyPosition::Bottom => 1,
        ReplyPosition::Follow => 2,
    }
}

/// The reply position value at row `idx` of its combo; past the end reads as the first.
pub(super) fn reply_position_at(idx: u32) -> crate::config::ReplyPosition {
    use crate::config::ReplyPosition;
    match idx {
        1 => ReplyPosition::Bottom,
        2 => ReplyPosition::Follow,
        _ => ReplyPosition::Top,
    }
}

/// The row of the section style combo: Unified, then Tree.
pub(super) fn section_style_index(value: crate::config::SectionStyle) -> u32 {
    u32::from(value == crate::config::SectionStyle::Tree)
}

/// The section style at row `idx`; anything but the second row is Unified.
pub(super) fn section_style_at(idx: u32) -> crate::config::SectionStyle {
    if idx == 1 {
        crate::config::SectionStyle::Tree
    } else {
        crate::config::SectionStyle::Unified
    }
}

#[cfg(test)]
mod tests {
    use super::super::system::link_group_description;
    use super::*;
    use crate::config::SectionPlacement;

    #[test]
    fn language_choices_start_with_system_and_english_then_linguas() {
        let choices = language_choices();
        assert_eq!(choices[0], ("System".to_string(), String::new()));
        assert_eq!(choices[1], ("English".to_string(), "en".to_string()));
        let codes: Vec<&str> = include_str!("../../../po/LINGUAS")
            .lines()
            .map(str::trim)
            .filter(|c| !c.is_empty() && !c.starts_with('#') && *c != "en")
            .collect();
        let listed: Vec<&str> = choices[2..].iter().map(|(_, c)| c.as_str()).collect();
        assert_eq!(listed, codes);
        for (name, code) in &choices[2..] {
            assert_eq!(name, native_language_name(code));
        }
    }

    #[test]
    fn languages_are_named_in_themselves() {
        assert_eq!(native_language_name("fr"), "Français");
        assert_eq!(native_language_name("pt_BR"), "Português (Brasil)");
        assert_eq!(native_language_name("nb"), "Norsk");
        assert_eq!(native_language_name("no"), "Norsk");
        assert_eq!(native_language_name("xx"), "xx");
    }

    #[test]
    fn section_placements_map_to_combo_rows_lossily() {
        use SectionPlacement::*;
        assert_eq!(placement_index(AllInboxes), 0);
        assert_eq!(placement_index(AboveAccounts), 0);
        assert_eq!(placement_index(Footer), 0);
        assert_eq!(placement_index(BelowAccounts), 1);
        assert_eq!(placement_index(Off), 2);
        assert_eq!(placement_from_index(0), AboveAccounts);
        assert_eq!(placement_from_index(1), BelowAccounts);
        assert_eq!(placement_from_index(2), Off);
        assert_eq!(placement_from_index(9), AboveAccounts);
        assert_eq!(contacts_placement_index(Footer), 0);
        assert_eq!(contacts_placement_index(AllInboxes), 1);
        assert_eq!(contacts_placement_index(AboveAccounts), 1);
        assert_eq!(contacts_placement_index(BelowAccounts), 2);
        assert_eq!(contacts_placement_index(Off), 3);
        assert_eq!(contacts_placement_from_index(0), Footer);
        assert_eq!(contacts_placement_from_index(1), AboveAccounts);
        assert_eq!(contacts_placement_from_index(2), BelowAccounts);
        assert_eq!(contacts_placement_from_index(3), Off);
        assert_eq!(contacts_placement_from_index(7), AboveAccounts);
    }

    #[test]
    fn link_browser_rows_are_default_ask_then_the_browsers() {
        let browsers = vec![
            crate::ui::launch::Browser {
                id: "firefox.desktop".into(),
                name: "Firefox".into(),
            },
            crate::ui::launch::Browser {
                id: "chromium.desktop".into(),
                name: "Chromium".into(),
            },
        ];
        assert_eq!(link_browser_index(&browsers, ""), 0);
        assert_eq!(link_browser_index(&browsers, crate::ui::launch::ASK), 1);
        assert_eq!(link_browser_index(&browsers, "firefox.desktop"), 2);
        assert_eq!(link_browser_index(&browsers, "chromium.desktop"), 3);
        assert_eq!(link_browser_index(&browsers, "gone.desktop"), 0);
        assert_eq!(link_browser_index(&[], "firefox.desktop"), 0);
        assert!(link_group_description(&[]).starts_with("Inside the Flatpak sandbox"));
        assert!(link_group_description(&browsers).starts_with("\"Ask each time\""));
    }

    #[test]
    fn files_choices_map_to_combo_rows() {
        use crate::config::{FilesAction, FilesLarge};
        assert_eq!(files_action_index(FilesAction::Ask), 0);
        assert_eq!(files_action_index(FilesAction::New), 1);
        assert_eq!(files_action_index(FilesAction::Draft), 2);
        assert_eq!(files_action_index(FilesAction::Reply), 3);
        assert_eq!(files_large_index(FilesLarge::Ask), 0);
        assert_eq!(files_large_index(FilesLarge::Attach), 1);
        assert_eq!(files_large_index(FilesLarge::Cloud), 2);
    }

    #[test]
    fn sounds_map_to_rows_with_custom_last() {
        assert_eq!(
            sound_labels(),
            ["Click", "Hum", "String", "Swing", "Custom File"]
        );
        assert_eq!(sound_index("click"), 0);
        assert_eq!(sound_index("swing"), 3);
        assert_eq!(sound_index(crate::config::CUSTOM_SOUND), 4);
        assert_eq!(sound_index("unknown"), 0);
        assert_eq!(sound_index(""), 0);
    }

    #[test]
    fn choice_tables_keep_their_order() {
        let tray: Vec<_> = TRAY_ICONS.iter().map(|(_, v)| *v).collect();
        assert_eq!(tray, [TrayIcon::Hylki, TrayIcon::Symbolic]);
        let badges: Vec<_> = LAUNCHER_BADGES.iter().map(|(_, v)| *v).collect();
        assert_eq!(
            badges,
            [
                LauncherBadge::Off,
                LauncherBadge::AllUnread,
                LauncherBadge::NewSinceFront
            ]
        );
        let themes: Vec<_> = APP_THEMES.iter().map(|(_, v)| *v).collect();
        assert_eq!(themes, [AppTheme::System, AppTheme::Light, AppTheme::Dark]);
        let msg: Vec<_> = MESSAGE_THEMES.iter().map(|(_, v)| *v).collect();
        assert_eq!(
            msg,
            [
                MessageTheme::System,
                MessageTheme::Light,
                MessageTheme::Dark
            ]
        );
        let dates: Vec<_> = DATE_STYLES.iter().map(|(_, v)| *v).collect();
        assert_eq!(
            dates,
            [
                DateStyle::System,
                DateStyle::MonthFirst,
                DateStyle::DayFirst,
                DateStyle::YearFirst
            ]
        );
        let clocks: Vec<_> = CLOCK_STYLES.iter().map(|(_, v)| *v).collect();
        assert_eq!(
            clocks,
            [
                ClockStyle::System,
                ClockStyle::Twelve,
                ClockStyle::TwentyFour
            ]
        );
        let secs: Vec<u64> = FETCH_INTERVALS.iter().map(|(_, s)| *s).collect();
        assert_eq!(secs, [0, 60, 300, 900, 1800]);
    }

    #[test]
    fn enum_rows_round_trip_and_fall_back_to_the_first() {
        use crate::config::*;
        for v in [
            StartView::AllInboxes,
            StartView::AccountInbox,
            StartView::LastFolder,
        ] {
            assert_eq!(start_view_at(start_view_index(v)), v);
        }
        assert_eq!(start_view_at(9), StartView::AllInboxes);
        for v in [
            ListLayout::Cards,
            ListLayout::SingleLine,
            ListLayout::Automatic,
        ] {
            assert_eq!(list_layout_at(list_layout_index(v)), v);
        }
        assert_eq!(list_layout_at(9), ListLayout::Cards);
        for v in [ReadMark::Shown, ReadMark::Delay, ReadMark::Manual] {
            assert_eq!(read_mark_at(read_mark_index(v)), v);
        }
        assert_eq!(read_mark_at(9), ReadMark::Shown);
        use crate::config::ReceiptPolicy;
        for v in [
            ReceiptPolicy::Ask,
            ReceiptPolicy::Always,
            ReceiptPolicy::Never,
        ] {
            assert_eq!(receipts_at(receipts_index(v)), v);
        }
        assert_eq!(receipts_at(9), ReceiptPolicy::Ask);
        for v in [
            ReturnedReceipts::Trash,
            ReturnedReceipts::Archive,
            ReturnedReceipts::Keep,
        ] {
            assert_eq!(returned_receipts_at(returned_receipts_index(v)), v);
        }
        assert_eq!(returned_receipts_at(9), ReturnedReceipts::Trash);
        for v in [
            FoldMessages::Never,
            FoldMessages::Read,
            FoldMessages::AllButNewest,
        ] {
            assert_eq!(fold_messages_at(fold_messages_index(v)), v);
        }
        assert_eq!(fold_messages_at(9), FoldMessages::Never);
        for v in [
            ReaderDefault::Remember,
            ReaderDefault::On,
            ReaderDefault::Off,
        ] {
            assert_eq!(reader_default_at(reader_default_index(v)), v);
        }
        assert_eq!(reader_default_at(9), ReaderDefault::Remember);
        for (i, v) in [
            ComposeFormat::Rich,
            ComposeFormat::Markdown,
            ComposeFormat::Html,
            ComposeFormat::Plain,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(compose_format_index(v), i as u32);
            assert_eq!(compose_format_at(i as u32), v);
        }
        assert_eq!(compose_format_at(9), ComposeFormat::Rich);
        for v in [SignaturePosition::AboveQuote, SignaturePosition::BelowQuote] {
            assert_eq!(signature_position_at(signature_position_index(v)), v);
        }
        assert_eq!(signature_position_at(9), SignaturePosition::AboveQuote);
        for v in [
            ReplyPosition::Top,
            ReplyPosition::Bottom,
            ReplyPosition::Follow,
        ] {
            assert_eq!(reply_position_at(reply_position_index(v)), v);
        }
        assert_eq!(reply_position_at(9), ReplyPosition::Top);
        assert_eq!(section_style_index(SectionStyle::Unified), 0);
        assert_eq!(section_style_index(SectionStyle::Tree), 1);
        assert_eq!(section_style_at(1), SectionStyle::Tree);
        assert_eq!(section_style_at(0), SectionStyle::Unified);
        assert_eq!(section_style_at(5), SectionStyle::Unified);
    }
}
