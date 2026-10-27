//! The smaller preferences kept in privacy.toml (reading, composing,
//! notifications, files, printing) and the accessors that read them.

use super::default_on;
use super::privacy::load_privacy;

/// What a printed page carries besides the message (#359), from Settings →
/// Reading → Printing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PrintOptions {
    /// "Page 2 of 5" at the foot of every page.
    #[serde(default = "default_on")]
    pub(crate) page_numbers: bool,
    /// The date and time it was printed, at the foot of every page.
    #[serde(default)]
    pub(crate) date: bool,
    /// A wider left margin, so a hole punch misses the text.
    #[serde(default)]
    pub(crate) punch_margin: bool,
}

impl Default for PrintOptions {
    fn default() -> Self {
        PrintOptions {
            page_numbers: true,
            date: false,
            punch_margin: false,
        }
    }
}

/// Where an account files the copy of a reply it sends (#468).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplyCopies {
    /// Wherever the rest of sent mail goes.
    #[default]
    WithSent,
    /// In the folder of the message replied to.
    Original,
    /// The same, but a reply to mail in the Inbox goes with sent mail:
    /// what the Inbox holds is waiting to be dealt with, and a reply is not.
    OriginalNotInbox,
}

impl ReplyCopies {
    pub(crate) const ALL: [ReplyCopies; 3] = [
        ReplyCopies::WithSent,
        ReplyCopies::Original,
        ReplyCopies::OriginalNotInbox,
    ];

    pub(crate) fn label(self) -> String {
        match self {
            ReplyCopies::WithSent => crate::i18n::i18n("With sent mail"),
            ReplyCopies::Original => crate::i18n::i18n("Folder of the original"),
            ReplyCopies::OriginalNotInbox => {
                crate::i18n::i18n("Folder of the original, not the Inbox")
            }
        }
    }

    /// Its place in [`ReplyCopies::ALL`], for combo rows.
    pub(crate) fn index(self) -> u32 {
        ReplyCopies::ALL
            .iter()
            .position(|s| *s == self)
            .unwrap_or(0) as u32
    }

    pub(super) fn is_default(&self) -> bool {
        *self == ReplyCopies::WithSent
    }
}

/// When an opened message is marked read (#100).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadMark {
    /// The moment it is shown (conversation members as they scroll into view).
    #[default]
    Shown,
    /// After it has been in view for a couple of seconds.
    Delay,
    /// Never automatically; only an explicit mark-as-read.
    Manual,
}

/// What a hand-off of files from GNOME Files ("Send with Hylki", "Open
/// With Hylki", "Email…") opens them into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilesAction {
    /// A dialog offers the three below, every time.
    #[default]
    Ask,
    /// A new message with the files attached.
    New,
    /// Pick one of the saved drafts and attach the files to it.
    Draft,
    /// Pick a message to reply to, with the files attached.
    Reply,
}

/// What happens to handed-in files that add up to more than the limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilesLarge {
    /// A dialog asks, every time.
    #[default]
    Ask,
    /// Attach them regardless.
    Attach,
    /// Upload them to cloud storage and put the links in the message.
    Cloud,
}

/// The GNOME Files hand-off preferences (Settings → System → GNOME Files).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FilesPrefs {
    pub(crate) action: FilesAction,
    pub(crate) large: FilesLarge,
    /// The size, in MB (decimal, like the sizes the composer shows), that
    /// the files together must stay under to be attached without a word.
    pub(crate) limit_mb: u32,
}

impl Default for FilesPrefs {
    fn default() -> Self {
        Self {
            action: FilesAction::Ask,
            large: FilesLarge::Ask,
            limit_mb: default_files_limit_mb(),
        }
    }
}

impl FilesPrefs {
    /// The limit in bytes.
    pub(crate) fn limit_bytes(&self) -> u64 {
        self.limit_mb as u64 * 1_000_000
    }
}

pub(super) fn default_files_limit_mb() -> u32 {
    20
}

pub(crate) fn load_files_prefs() -> FilesPrefs {
    let f = load_privacy();
    FilesPrefs {
        action: f.files_action,
        large: f.files_large,
        limit_mb: f.files_limit_mb.max(1),
    }
}

/// The zoom steps Ctrl+ and Ctrl- walk, and Settings offers, in percent.
pub(crate) const READER_ZOOM_STEPS: [u32; 12] =
    [50, 60, 70, 80, 90, 100, 110, 125, 150, 175, 200, 250];

pub(super) fn default_reader_zoom() -> u32 {
    100
}

/// The zoom a new message opens at: Settings → Composing's, or the
/// reader's default while that says "Same as reading pane zoom" (0) or anything off
/// the step list.
pub(crate) fn load_compose_zoom() -> u32 {
    let z = load_privacy().compose_zoom;
    if READER_ZOOM_STEPS.contains(&z) {
        z
    } else {
        load_reader_zoom()
    }
}

/// The message zoom a launch starts at; anything off the step list reads
/// as 100.
pub(crate) fn load_reader_zoom() -> u32 {
    let z = load_privacy().reader_zoom;
    if READER_ZOOM_STEPS.contains(&z) {
        z
    } else {
        100
    }
}

/// What Reader View does each time a message is opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReaderDefault {
    /// Stay as last set; the switch's state is remembered across runs.
    #[default]
    Remember,
    /// Every message opens in Reader View (the switch can turn it off for
    /// that one).
    On,
    /// Every message opens as sent (the switch can turn Reader View on for
    /// that one).
    Off,
}

impl ReaderDefault {
    /// The state a freshly opened message starts in, or `None` to keep the
    /// last choice.
    pub(crate) fn starts_on(self) -> Option<bool> {
        match self {
            ReaderDefault::Remember => None,
            ReaderDefault::On => Some(true),
            ReaderDefault::Off => Some(false),
        }
    }
}

/// The three reader-override preferences (#56): font switch, font, color switch.
pub(crate) fn load_reader_override() -> (bool, String, bool) {
    let p = load_privacy();
    (p.override_fonts, p.reader_font, p.override_colors)
}

/// Plain-text messages in monospace (#181): the switch and the font.
pub(crate) fn load_plain_style() -> (bool, String) {
    let p = load_privacy();
    (p.plain_monospace, p.plain_font)
}

/// What a message is written in. Rich text is the WYSIWYG editor;
/// Markdown and HTML are source views that are converted on the way out;
/// plain text sends no HTML part at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComposeFormat {
    #[default]
    Rich,
    Markdown,
    Html,
    Plain,
}

impl ComposeFormat {
    /// Whether this format is edited as source rather than as rich text.
    pub(crate) fn is_source(self) -> bool {
        matches!(self, ComposeFormat::Markdown | ComposeFormat::Html)
    }
}

/// Which messages of a conversation open folded to one line (#326). The
/// newest always opens; a folded one opens with a click.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FoldMessages {
    /// Every message opens.
    #[default]
    Never,
    /// The messages already read.
    Read,
    /// Every message but the newest, read or not.
    AllButNewest,
}

/// Where the split reply opens in the reading pane (#212): above the
/// messages (the original placement), below them, or wherever the newest
/// message is — above with "newest message first", below otherwise, so the
/// editor always continues the conversation in its reading direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplyPosition {
    #[default]
    Top,
    Bottom,
    Follow,
}

/// Where the signature goes in a reply or forward (#237). Above the quoted
/// original is what Apple Mail and Thunderbird do, and what a reader
/// expects: the signature closes the words above it, not the words
/// somebody else wrote. Below is the placement Hylki had before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SignaturePosition {
    #[default]
    AboveQuote,
    BelowQuote,
}

/// What the number on the launcher icon counts (#431).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LauncherBadge {
    /// No badge.
    Off,
    /// Every unread message in the inboxes, as the tray icon's dot.
    #[default]
    AllUnread,
    /// Inbox mail that arrived while the main window was not in front and
    /// is still unread; back to none each time the window comes forward.
    NewSinceFront,
}

/// What new messages start out as, falling back to the plain-text
/// switch this setting replaced (#180).
pub(crate) fn load_compose_format() -> ComposeFormat {
    let p = load_privacy();
    p.compose_format.unwrap_or(if p.compose_plain {
        ComposeFormat::Plain
    } else {
        ComposeFormat::Rich
    })
}

/// One of the action buttons a new-mail notification can carry (#244).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotificationButton {
    MarkRead,
    Archive,
    Delete,
    Reply,
    Forward,
    Spam,
}

impl NotificationButton {
    /// Every button, in the order they appear on a notification.
    pub(crate) const ALL: [NotificationButton; 6] = [
        NotificationButton::MarkRead,
        NotificationButton::Archive,
        NotificationButton::Delete,
        NotificationButton::Reply,
        NotificationButton::Forward,
        NotificationButton::Spam,
    ];

    /// The name the setting is stored under.
    fn key(self) -> &'static str {
        match self {
            NotificationButton::MarkRead => "mark_read",
            NotificationButton::Archive => "archive",
            NotificationButton::Delete => "delete",
            NotificationButton::Reply => "reply",
            NotificationButton::Forward => "forward",
            NotificationButton::Spam => "spam",
        }
    }
}

/// Which action buttons a new-mail notification carries (#244): any three
/// of the six, chosen in Settings → General → Notifications. Three is what
/// GNOME's notification portal shows; a fourth would be dropped anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotificationButtons {
    pub(crate) mark_read: bool,
    pub(crate) archive: bool,
    pub(crate) delete: bool,
    pub(crate) reply: bool,
    pub(crate) forward: bool,
    pub(crate) spam: bool,
}

impl NotificationButtons {
    pub(crate) const MAX: usize = 3;

    pub(super) const NONE: Self = Self {
        mark_read: false,
        archive: false,
        delete: false,
        reply: false,
        forward: false,
        spam: false,
    };
}

impl Default for NotificationButtons {
    fn default() -> Self {
        Self {
            mark_read: true,
            archive: true,
            delete: true,
            ..Self::NONE
        }
    }
}

impl NotificationButtons {
    pub(crate) fn get(self, button: NotificationButton) -> bool {
        match button {
            NotificationButton::MarkRead => self.mark_read,
            NotificationButton::Archive => self.archive,
            NotificationButton::Delete => self.delete,
            NotificationButton::Reply => self.reply,
            NotificationButton::Forward => self.forward,
            NotificationButton::Spam => self.spam,
        }
    }

    pub(crate) fn set(&mut self, button: NotificationButton, on: bool) {
        match button {
            NotificationButton::MarkRead => self.mark_read = on,
            NotificationButton::Archive => self.archive = on,
            NotificationButton::Delete => self.delete = on,
            NotificationButton::Reply => self.reply = on,
            NotificationButton::Forward => self.forward = on,
            NotificationButton::Spam => self.spam = on,
        }
    }

    /// How many are on.
    pub(crate) fn count(self) -> usize {
        NotificationButton::ALL
            .into_iter()
            .filter(|b| self.get(*b))
            .count()
    }

    /// Whether the set is full, so another button cannot be switched on.
    pub(crate) fn full(self) -> bool {
        self.count() >= Self::MAX
    }

    /// The setting as stored: the names of the buttons that are on. A name
    /// the file has that this version does not know is dropped, and past
    /// the third button (in [`NotificationButton::ALL`] order) so are the
    /// rest.
    pub(super) fn from_list(names: &[String]) -> Self {
        let mut b = Self::NONE;
        for button in NotificationButton::ALL {
            if !b.full() && names.iter().any(|n| n == button.key()) {
                b.set(button, true);
            }
        }
        b
    }

    pub(crate) fn to_list(self) -> Vec<String> {
        NotificationButton::ALL
            .into_iter()
            .filter(|b| self.get(*b))
            .map(|b| b.key().to_string())
            .collect()
    }
}

/// Which action buttons a new-mail notification carries (#244).
pub(crate) fn load_notification_buttons() -> NotificationButtons {
    NotificationButtons::from_list(&load_privacy().notification_buttons)
}

/// The narrowest and widest trackpad swipe sensitivity the setting offers,
/// also the clamp a hand-edited file is held to (0 would divide by zero).
pub(crate) const SWIPE_SENSITIVITY_MIN: f64 = 1.0;
pub(crate) const SWIPE_SENSITIVITY_MAX: f64 = 10.0;

pub(super) fn default_swipe_sensitivity() -> f64 {
    // libadwaita spends a fixed 400px of scroll on a full swipe whatever the
    // row asks for, so at 1.0 a trackpad needs 240px of two-finger travel to
    // reach the commit distance — more than most touchpads can give in one
    // go. 3.5 puts the row roughly under the fingers instead.
    3.5
}

/// How far a trackpad's two-finger swipe has to travel to fire the action
/// (higher = shorter swipe).
pub(crate) fn load_swipe_sensitivity() -> f64 {
    load_privacy()
        .swipe_sensitivity
        .clamp(SWIPE_SENSITIVITY_MIN, SWIPE_SENSITIVITY_MAX)
}

/// The delays Settings → Composing offers for Undo Send (#434), in seconds.
pub(crate) const UNDO_SEND_CHOICES: [u32; 6] = [0, 3, 5, 10, 20, 30];

pub(super) fn default_undo_send_secs() -> u32 {
    5
}

/// The address book chosen for new contacts (empty: automatic).
pub(crate) fn load_contact_book() -> String {
    load_privacy().contact_book
}

/// Whether Hylki starts at login (background running only).
pub(crate) fn load_autostart() -> bool {
    let p = load_privacy();
    p.run_in_background && p.autostart
}

#[cfg(test)]
mod files_prefs_tests {
    use super::*;
    use crate::config::PrivacyFile;

    #[test]
    fn missing_keys_default_to_asking() {
        let file: PrivacyFile = toml::from_str("compose_inline = true\n").unwrap();
        assert_eq!(file.files_action, FilesAction::Ask);
        assert_eq!(file.files_large, FilesLarge::Ask);
        assert_eq!(file.files_limit_mb, 20);
    }

    #[test]
    fn keys_round_trip_by_name() {
        let file: PrivacyFile = toml::from_str(
            "files_action = \"reply\"\nfiles_large = \"cloud\"\nfiles_limit_mb = 50\n",
        )
        .unwrap();
        assert_eq!(file.files_action, FilesAction::Reply);
        assert_eq!(file.files_large, FilesLarge::Cloud);
        assert_eq!(file.files_limit_mb, 50);
        let text = toml::to_string(&file).unwrap();
        assert!(text.contains("files_action = \"reply\""), "{text}");
        assert!(text.contains("files_large = \"cloud\""), "{text}");
        let prefs = FilesPrefs {
            action: FilesAction::New,
            large: FilesLarge::Attach,
            limit_mb: 3,
        };
        assert_eq!(prefs.limit_bytes(), 3_000_000);
    }
}
