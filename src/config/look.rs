//! How the app looks: theme, tray icon, message and date styles, text
//! size, and the message list's layout and columns.

use serde::{Deserialize, Serialize};

use super::privacy::load_privacy;

/// The app's own theme: follow the system, or force light/dark regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AppTheme {
    #[default]
    System,
    Light,
    Dark,
}

/// Which icon the tray item wears (issue #116).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TrayIcon {
    /// The app icon.
    Hylki,
    /// The app's symbolic icon, which the panel draws in its own color, as
    /// it does the icons beside it (#258). It replaced a white and a black
    /// envelope, which read as it.
    #[default]
    #[serde(alias = "envelope-light", alias = "envelope-dark")]
    Symbolic,
}

/// How email message content is themed, independent of the app UI theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MessageTheme {
    /// Follow the system / app light-dark preference.
    #[default]
    System,
    Light,
    Dark,
}

impl MessageTheme {
    /// Forced dark flag for message content, or `None` to follow the system.
    pub(crate) fn dark_override(self) -> Option<bool> {
        match self {
            MessageTheme::System => None,
            MessageTheme::Light => Some(false),
            MessageTheme::Dark => Some(true),
        }
    }
}

/// The reader's own typography and colors, laid over the sender's (#56).
///
/// What the reader applies: `font` is the Pango description to set mail in
/// (`None` = the sender's fonts stand), `colors` forces the reader's own text
/// and ground colors over the sender's. Built by the app from the three
/// stored preferences, with the interface font filled in where none was
/// chosen, so the reader never has to ask GTK.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct ReaderStyle {
    /// Pango description ("Cantarell 11") of the font every message is set
    /// in; `None` leaves the sender's fonts and sizes alone.
    pub(crate) font: Option<String>,
    /// Ignore the sender's text and background colors.
    pub(crate) colors: bool,
    /// Pango description of the font plain-text messages are set in
    /// (#181); `None` leaves them in the reader's default.
    pub(crate) plain_font: Option<String>,
}

impl ReaderStyle {
    /// The sender's formatting stands entirely.
    pub(crate) const NONE: ReaderStyle = ReaderStyle {
        font: None,
        colors: false,
        plain_font: None,
    };

    /// Whether anything of the sender's is overridden.
    pub(crate) fn active(&self) -> bool {
        self.font.is_some() || self.colors || self.plain_font.is_some()
    }
}

/// How dates are written: the system's own arrangement, or one the user picked
/// regardless of it (#32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DateStyle {
    /// Follow the locale: its field order, month names and separators.
    #[default]
    System,
    /// Aug 23, 2026
    MonthFirst,
    /// 23 Aug 2026
    DayFirst,
    /// 2026 Aug 23
    YearFirst,
}

/// Whether the clock runs to 12 or 24, or follows the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ClockStyle {
    #[default]
    System,
    /// 5:40 PM
    Twelve,
    /// 17:40
    TwentyFour,
}

/// How dates are written, and on what clock.
pub(crate) fn load_date_format() -> (DateStyle, ClockStyle) {
    let p = load_privacy();
    (p.date_style, p.clock_style)
}

pub(super) fn default_text_scale() -> u32 {
    100
}

/// The app's text size in percent; anything Settings does not offer reads
/// as the desktop's own size.
pub(crate) fn load_text_scale() -> u32 {
    let percent = load_privacy().text_scale;
    if crate::desktop::text_scale::STEPS.contains(&percent) {
        percent
    } else {
        100
    }
}

/// The appearance theme's id; "system" (the stock look) when unset.
pub(crate) fn load_theme() -> String {
    let id = load_privacy().theme;
    if id.is_empty() {
        crate::desktop::theme::SYSTEM_ID.to_string()
    } else {
        id
    }
}

/// Where the reader sits (#475): beside the list, below it, or nowhere,
/// messages then opening in windows of their own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", from = "ReadingPaneSaved")]
pub(crate) enum ReadingPane {
    #[default]
    Right,
    Below,
    Off,
}

/// What a settings file may hold: the pane's name, or the on/off switch
/// that came before Below and was never released.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ReadingPaneSaved {
    Name(String),
    On(bool),
}

impl From<ReadingPaneSaved> for ReadingPane {
    fn from(saved: ReadingPaneSaved) -> Self {
        match saved {
            ReadingPaneSaved::Name(key) => ReadingPane::from_key(&key).unwrap_or_default(),
            ReadingPaneSaved::On(true) => ReadingPane::Right,
            ReadingPaneSaved::On(false) => ReadingPane::Off,
        }
    }
}

impl ReadingPane {
    /// The main menu's name for it, the action's state.
    pub(crate) fn key(self) -> &'static str {
        match self {
            ReadingPane::Right => "right",
            ReadingPane::Below => "below",
            ReadingPane::Off => "off",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        [ReadingPane::Right, ReadingPane::Below, ReadingPane::Off]
            .into_iter()
            .find(|p| p.key() == key)
    }
}

/// How the message list lays out a message (#334).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ListLayout {
    /// Sender, subject and preview on lines of their own.
    #[default]
    Cards,
    /// One line per message, in columns.
    SingleLine,
    /// One line when the list pane is wide, cards when it is narrow.
    Automatic,
}

pub(crate) fn load_list_layout() -> ListLayout {
    load_privacy().list_layout
}

/// One column of the single-line message list (#334).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ListColumn {
    Star,
    /// Replied to, forwarded, or both (#454).
    Replied,
    Sender,
    Recipients,
    Correspondents,
    Subject,
    Tags,
    Attachment,
    Importance,
    Account,
    /// A Microsoft 365 follow-up flag's due date, the one kind of mail
    /// that has one: the column takes no room in a list without any.
    Due,
    Date,
}

impl ListColumn {
    pub(crate) const ALL: [ListColumn; 12] = [
        ListColumn::Star,
        ListColumn::Replied,
        ListColumn::Sender,
        ListColumn::Recipients,
        ListColumn::Correspondents,
        ListColumn::Subject,
        ListColumn::Tags,
        ListColumn::Attachment,
        ListColumn::Importance,
        ListColumn::Account,
        ListColumn::Due,
        ListColumn::Date,
    ];

    /// The columns a single line shows until they are changed: the ones it
    /// had before they could be, and the replied arrows (#454).
    pub(crate) const DEFAULT: [ListColumn; 7] = [
        ListColumn::Star,
        ListColumn::Replied,
        ListColumn::Sender,
        ListColumn::Subject,
        ListColumn::Tags,
        ListColumn::Attachment,
        ListColumn::Date,
    ];

    /// The stable name written to `privacy.toml`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            ListColumn::Star => "star",
            ListColumn::Replied => "replied",
            ListColumn::Sender => "sender",
            ListColumn::Recipients => "recipients",
            ListColumn::Correspondents => "correspondents",
            ListColumn::Subject => "subject",
            ListColumn::Tags => "tags",
            ListColumn::Attachment => "attachment",
            ListColumn::Importance => "importance",
            ListColumn::Account => "account",
            ListColumn::Due => "due",
            ListColumn::Date => "date",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<ListColumn> {
        ListColumn::ALL.iter().copied().find(|c| c.key() == key)
    }

    /// The icon on the column's chip in Settings.
    pub(crate) fn icon(self) -> &'static str {
        match self {
            ListColumn::Star => "starred-symbolic",
            ListColumn::Replied => "mail-reply-sender-symbolic",
            ListColumn::Sender => "avatar-default-symbolic",
            ListColumn::Recipients => "mail-send-symbolic",
            ListColumn::Correspondents => "chat-bubbles-text-symbolic",
            ListColumn::Subject => "text-x-generic-symbolic",
            ListColumn::Tags => "tag-outline-symbolic",
            ListColumn::Attachment => "mail-attachment-symbolic",
            ListColumn::Importance => "emblem-important-symbolic",
            ListColumn::Account => "mail-inbox-symbolic",
            ListColumn::Due => "alarm-symbolic",
            ListColumn::Date => "x-office-calendar-symbolic",
        }
    }

    /// The untranslated label (callers pass it through `i18n`).
    pub(crate) fn label(self) -> &'static str {
        match self {
            ListColumn::Star => crate::i18n::i18n_noop("Star"),
            ListColumn::Replied => crate::i18n::i18n_noop("Replied"),
            ListColumn::Sender => crate::i18n::i18n_noop("Sender"),
            ListColumn::Recipients => crate::i18n::i18n_noop("Recipients"),
            ListColumn::Correspondents => crate::i18n::i18n_noop("Correspondents"),
            ListColumn::Subject => crate::i18n::i18n_noop("Subject"),
            ListColumn::Tags => crate::i18n::i18n_noop("Tags"),
            ListColumn::Attachment => crate::i18n::i18n_noop("Attachment"),
            ListColumn::Importance => crate::i18n::i18n_noop("Importance"),
            ListColumn::Account => crate::i18n::i18n_noop("Account"),
            ListColumn::Due => crate::i18n::i18n_noop("Due Date"),
            ListColumn::Date => crate::i18n::i18n_noop("Date"),
        }
    }

    /// The shown columns, in order, from their saved names. Names this
    /// version does not know are skipped, and the subject is always there:
    /// it is the column that takes the room the others leave.
    pub(crate) fn from_keys(keys: &[String]) -> Vec<ListColumn> {
        let mut out: Vec<ListColumn> = Vec::new();
        for c in keys.iter().filter_map(|k| ListColumn::from_key(k)) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        if !out.contains(&ListColumn::Subject) {
            let at = out
                .iter()
                .position(|c| *c == ListColumn::Sender)
                .map(|i| i + 1)
                .unwrap_or(0);
            out.insert(at, ListColumn::Subject);
        }
        out
    }

    pub(crate) fn to_keys(columns: &[ListColumn]) -> Vec<String> {
        columns.iter().map(|c| c.key().to_string()).collect()
    }
}

pub(super) fn default_list_columns() -> Vec<String> {
    ListColumn::to_keys(&ListColumn::DEFAULT)
}

/// The widths single-line columns were dragged to (#334), by column.
pub(crate) fn load_list_column_widths() -> std::collections::HashMap<ListColumn, i32> {
    list_column_widths_from(&load_privacy().list_column_widths)
}

pub(super) fn list_column_widths_from(
    saved: &std::collections::BTreeMap<String, i32>,
) -> std::collections::HashMap<ListColumn, i32> {
    saved
        .iter()
        .filter_map(|(k, px)| Some((ListColumn::from_key(k)?, (*px).clamp(32, 800))))
        .collect()
}

pub(crate) fn list_column_widths_to(
    widths: &std::collections::HashMap<ListColumn, i32>,
) -> std::collections::BTreeMap<String, i32> {
    widths
        .iter()
        .map(|(c, px)| (c.key().to_string(), *px))
        .collect()
}

/// The single-line list's columns, in order (#334).
pub(crate) fn load_list_columns() -> Vec<ListColumn> {
    ListColumn::from_keys(&load_privacy().list_columns)
}

/// Lines of message text shown under the subject in the list; 0 means previews
/// are off. Clamped in case the file was edited by hand.
pub(crate) fn load_preview_lines() -> u32 {
    load_privacy().preview_lines.min(3)
}
