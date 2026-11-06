//! Focus Mode and focus.toml.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{
    config_file, default_on, read_toml, save_plain_toml, ListLayout, RailFold, SectionPlacement,
    UnifiedChips, UnifiedKinds,
};

// ---- Focus Mode -----------------------------------------------------------

/// Focus Mode: a distraction-free layout for reading, switched on from the
/// main menu, Ctrl+Shift+F or Settings → Focus Mode. While it is on, these
/// fields alone decide the layout of the sidebar, the message list and the
/// reader: the ordinary settings for them wait until it ends, so everything
/// about the mode is set in one place. Stored in focus.toml (like the toolbar
/// layout) so the ordinary settings file and its long save call stay as they
/// are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FocusMode {
    /// The master switch: on, the parts below apply.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Whether the app opens in Focus Mode. Not a part — it strips nothing
    /// itself, it decides what [`Self::enabled`] starts each launch as, so
    /// the mode is a deliberate choice rather than whatever the last
    /// session happened to leave behind.
    #[serde(default)]
    pub(crate) start_focused: bool,
    /// Every button of the reading pane's toolbar folds into its ⋯ menu.
    #[serde(default = "default_on")]
    pub(crate) reader_toolbar: bool,
    /// The message list's search, filters, count and sort fold into a ⋯
    /// menu; only the sidebar toggle stays.
    #[serde(default = "default_on")]
    pub(crate) list_header: bool,
    /// The sidebar's account sections are hidden.
    #[serde(default = "default_on")]
    pub(crate) hide_accounts: bool,
    /// The sidebar's unified rows (Inboxes, Starred, …) fold up.
    #[serde(default = "default_on")]
    pub(crate) fold_unified: bool,
    /// The message list's sender avatars are hidden.
    #[serde(default = "default_on")]
    pub(crate) hide_avatars: bool,
    /// How many lines of preview text the message list shows, 0 to 3. Unset
    /// in a file saved before this setting: see [`Self::preview_line_count`].
    pub(crate) preview_lines: Option<u32>,
    /// The older switches [`Self::preview_lines`] replaced. Read so a file
    /// that has them keeps its choice, and never written again.
    #[serde(default = "default_on", skip_serializing)]
    pub(crate) one_preview_line: bool,
    #[serde(default = "default_on", skip_serializing)]
    pub(crate) hide_preview: bool,
    /// The message list shows no subject either: the sender, the date and
    /// the row's own marks, and nothing else. Off unless it is asked for —
    /// the subject is how most people find a message again, so this one is
    /// a step past what the mode does by default.
    #[serde(default)]
    pub(crate) hide_subject: bool,
    /// The message list's rows show no star, and no column for it on one line.
    #[serde(default)]
    pub(crate) hide_stars: bool,
    /// The message list's rows show no paperclip, and no column for it on one
    /// line.
    #[serde(default)]
    pub(crate) hide_attachment_icons: bool,
    /// The sidebar shows as the icon rail, with unread dots in place of the
    /// counts. The user's own rail choice is untouched: this is on screen
    /// for as long as Focus Mode is, and the sidebar comes back as it was.
    #[serde(default = "default_on")]
    pub(crate) rail_sidebar: bool,
    /// Every message opens in Reader View (the switch still works).
    #[serde(default = "default_on")]
    pub(crate) reader_view: bool,
    /// The rail marks unread folders with a dot instead of a count.
    #[serde(default = "default_on")]
    pub(crate) rail_dots: bool,
    /// Hovering the rail slides the full sidebar out.
    #[serde(default)]
    pub(crate) hover_expand: bool,
    /// The accounts show folded in the rail, their avatars alone.
    #[serde(default = "default_on")]
    pub(crate) fold_accounts: bool,
    /// A click on an account folds it. Off, as in the rail's default, a click
    /// on its avatar opens its inbox.
    #[serde(default)]
    pub(crate) click_folds: bool,
    /// The unified rows wear unread counts.
    #[serde(default = "default_on")]
    pub(crate) unread_counts: bool,
    /// The sections of the sidebar. Above the accounts while they show; only
    /// Contacts sits in the footer, with Attachments.
    #[serde(default = "default_on")]
    pub(crate) show_favorites: bool,
    #[serde(default = "default_on")]
    pub(crate) show_filters: bool,
    #[serde(default = "default_on")]
    pub(crate) show_tags: bool,
    #[serde(default = "default_on")]
    pub(crate) show_contacts: bool,
    #[serde(default = "default_on")]
    pub(crate) show_attachments: bool,
    /// The All Inboxes row.
    #[serde(default = "default_on")]
    pub(crate) show_inboxes: bool,
    /// A card's tags and labels go on a line of their own under its text.
    #[serde(default)]
    pub(crate) tags_below: bool,
    /// How the message list lays out a message.
    #[serde(default)]
    pub(crate) layout: ListLayout,
    /// The unified rows besides All Inboxes. A table in the file, so it stays
    /// the last field: TOML wants plain values ahead of tables.
    #[serde(default)]
    pub(crate) unified: UnifiedKinds,
}

impl Default for FocusMode {
    fn default() -> Self {
        FocusMode {
            enabled: false,
            start_focused: false,
            reader_toolbar: true,
            list_header: true,
            hide_accounts: true,
            fold_unified: true,
            hide_avatars: true,
            preview_lines: None,
            one_preview_line: true,
            hide_preview: true,
            hide_subject: false,
            hide_stars: false,
            hide_attachment_icons: false,
            rail_sidebar: true,
            reader_view: true,
            rail_dots: true,
            hover_expand: false,
            fold_accounts: true,
            click_folds: false,
            unread_counts: true,
            show_favorites: true,
            show_filters: true,
            show_tags: true,
            show_contacts: true,
            show_attachments: true,
            show_inboxes: true,
            tags_below: false,
            layout: ListLayout::default(),
            unified: UnifiedKinds::default(),
        }
    }
}

/// One switch of Focus Mode, for Settings rows that all set the same struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusPart {
    Enabled,
    /// Not a layout part: the launch switch (see [`FocusMode::start_focused`]).
    /// It rides here so Settings can set it the same way as the rest;
    /// [`FocusMode::active`] is meaningless for it and nothing asks.
    StartFocused,
    ReaderToolbar,
    ListHeader,
    HideAccounts,
    FoldUnified,
    HideAvatars,
    HideSubject,
    HideStars,
    HideAttachmentIcons,
    RailSidebar,
    ReaderView,
    RailDots,
    HoverExpand,
    FoldAccounts,
    ClickFolds,
    UnreadCounts,
    ShowFavorites,
    ShowFilters,
    ShowTags,
    ShowContacts,
    ShowAttachments,
    ShowInboxes,
    TagsBelow,
    Starred,
    Sent,
    Drafts,
    Archive,
    Junk,
    Trash,
    Snoozed,
}

impl FocusMode {
    pub(crate) fn get(&self, part: FocusPart) -> bool {
        match part {
            FocusPart::Enabled => self.enabled,
            FocusPart::StartFocused => self.start_focused,
            FocusPart::ReaderToolbar => self.reader_toolbar,
            FocusPart::ListHeader => self.list_header,
            FocusPart::HideAccounts => self.hide_accounts,
            FocusPart::FoldUnified => self.fold_unified,
            FocusPart::HideAvatars => self.hide_avatars,
            FocusPart::HideSubject => self.hide_subject,
            FocusPart::HideStars => self.hide_stars,
            FocusPart::HideAttachmentIcons => self.hide_attachment_icons,
            FocusPart::RailSidebar => self.rail_sidebar,
            FocusPart::ReaderView => self.reader_view,
            FocusPart::RailDots => self.rail_dots,
            FocusPart::HoverExpand => self.hover_expand,
            FocusPart::FoldAccounts => self.fold_accounts,
            FocusPart::ClickFolds => self.click_folds,
            FocusPart::UnreadCounts => self.unread_counts,
            FocusPart::ShowFavorites => self.show_favorites,
            FocusPart::ShowFilters => self.show_filters,
            FocusPart::ShowTags => self.show_tags,
            FocusPart::ShowContacts => self.show_contacts,
            FocusPart::ShowAttachments => self.show_attachments,
            FocusPart::ShowInboxes => self.show_inboxes,
            FocusPart::TagsBelow => self.tags_below,
            FocusPart::Starred => self.unified.starred,
            FocusPart::Sent => self.unified.sent,
            FocusPart::Drafts => self.unified.drafts,
            FocusPart::Archive => self.unified.archive,
            FocusPart::Junk => self.unified.junk,
            FocusPart::Trash => self.unified.trash,
            FocusPart::Snoozed => self.unified.snoozed,
        }
    }

    pub(crate) fn set(&mut self, part: FocusPart, on: bool) {
        match part {
            FocusPart::Enabled => self.enabled = on,
            FocusPart::StartFocused => self.start_focused = on,
            FocusPart::ReaderToolbar => self.reader_toolbar = on,
            FocusPart::ListHeader => self.list_header = on,
            FocusPart::HideAccounts => self.hide_accounts = on,
            FocusPart::FoldUnified => self.fold_unified = on,
            FocusPart::HideAvatars => self.hide_avatars = on,
            FocusPart::HideSubject => self.hide_subject = on,
            FocusPart::HideStars => self.hide_stars = on,
            FocusPart::HideAttachmentIcons => self.hide_attachment_icons = on,
            FocusPart::RailSidebar => self.rail_sidebar = on,
            FocusPart::ReaderView => self.reader_view = on,
            FocusPart::RailDots => self.rail_dots = on,
            FocusPart::HoverExpand => self.hover_expand = on,
            FocusPart::FoldAccounts => self.fold_accounts = on,
            FocusPart::ClickFolds => self.click_folds = on,
            FocusPart::UnreadCounts => self.unread_counts = on,
            FocusPart::ShowFavorites => self.show_favorites = on,
            FocusPart::ShowFilters => self.show_filters = on,
            FocusPart::ShowTags => self.show_tags = on,
            FocusPart::ShowContacts => self.show_contacts = on,
            FocusPart::ShowAttachments => self.show_attachments = on,
            FocusPart::ShowInboxes => self.show_inboxes = on,
            FocusPart::TagsBelow => self.tags_below = on,
            FocusPart::Starred => self.unified.starred = on,
            FocusPart::Sent => self.unified.sent = on,
            FocusPart::Drafts => self.unified.drafts = on,
            FocusPart::Archive => self.unified.archive = on,
            FocusPart::Junk => self.unified.junk = on,
            FocusPart::Trash => self.unified.trash = on,
            FocusPart::Snoozed => self.unified.snoozed = on,
        }
    }

    /// How many lines of preview text the list shows. A file from before the
    /// setting keeps what its switches said; with both off they followed the
    /// list's own setting, whose default is one line.
    pub(crate) fn preview_line_count(&self) -> u32 {
        self.preview_lines
            .map_or_else(|| u32::from(!self.hide_preview), |lines| lines.min(3))
    }

    /// This with its preview lines written out. The older switches are not
    /// saved, so a file from before them would lose what they said.
    fn with_preview_lines_set(&self) -> Self {
        Self {
            preview_lines: Some(self.preview_line_count()),
            ..*self
        }
    }

    /// A section's place in the sidebar: above the accounts, or left out.
    pub(crate) fn section_placement(on: bool) -> SectionPlacement {
        if on {
            SectionPlacement::AboveAccounts
        } else {
            SectionPlacement::Off
        }
    }

    /// Contacts' place in the sidebar: the footer, or left out.
    pub(crate) fn contacts_placement(&self) -> SectionPlacement {
        if self.show_contacts {
            SectionPlacement::Footer
        } else {
            SectionPlacement::Off
        }
    }

    /// The accounts fold for the rail by this mode's switch; the unified rows
    /// fold by [`Self::fold_unified`], so none of them does here.
    pub(crate) fn rail_fold(&self) -> RailFold {
        RailFold {
            enabled: true,
            accounts: self.fold_accounts,
            all_inboxes: false,
            starred: false,
            sent: false,
            drafts: false,
            archive: false,
            junk: false,
            trash: false,
            snoozed: false,
            filtered: false,
            tags: false,
        }
    }

    /// The unified rows' unread chips: one switch for all of them.
    pub(crate) fn unified_chips(&self) -> UnifiedChips {
        let on = self.unread_counts;
        UnifiedChips {
            all_inboxes: on,
            starred: on,
            drafts: on,
            archive: on,
            junk: on,
            trash: on,
            snoozed: on,
        }
    }

    /// Whether `part` is in force: Focus Mode is on and the part is ticked.
    pub(crate) fn active(&self, part: FocusPart) -> bool {
        self.enabled && self.get(part)
    }
}

fn focus_path() -> Option<PathBuf> {
    config_file("focus.toml")
}

/// The saved Focus Mode settings, or the defaults (off) when there are none.
pub(crate) fn load_focus_mode() -> FocusMode {
    read_toml::<FocusMode>(focus_path()).unwrap_or_default()
}

pub(crate) fn save_focus_mode(focus: &FocusMode) {
    save_plain_toml(focus_path(), &focus.with_preview_lines_set());
}

#[cfg(test)]
mod focus_tests {
    use super::*;

    #[test]
    fn focus_defaults_off_with_every_part_ticked() {
        let f = FocusMode::default();
        assert!(!f.enabled);
        // The app opens out of the mode until it is asked to do otherwise.
        assert!(!f.start_focused);
        assert!(f.reader_toolbar && f.list_header && f.hide_accounts && f.fold_unified);
        assert!(f.hide_avatars && f.one_preview_line && f.reader_view);
        assert!(f.hide_preview && f.rail_sidebar);
        // The exception: the subject stays until it is asked for.
        assert!(!f.hide_subject);
        // Off, no part is in force whatever its switch says.
        assert!(!f.active(FocusPart::ReaderView));
    }

    #[test]
    fn a_file_from_before_preview_lines_keeps_its_switches() {
        let read = |text: &str| {
            toml::from_str::<FocusMode>(text)
                .unwrap()
                .preview_line_count()
        };
        assert_eq!(read(""), 0, "hide_preview is the default");
        assert_eq!(read("hide_preview = true\none_preview_line = true"), 0);
        assert_eq!(read("hide_preview = false\none_preview_line = true"), 1);
        assert_eq!(read("hide_preview = false\none_preview_line = false"), 1);
        assert_eq!(read("preview_lines = 2\nhide_preview = true"), 2);
        assert_eq!(read("preview_lines = 9"), 3, "more than the list has");
    }

    #[test]
    fn saving_a_file_from_before_preview_lines_keeps_what_it_said() {
        let old: FocusMode =
            toml::from_str("hide_preview = false\none_preview_line = true").unwrap();
        let text = toml::to_string(&old.with_preview_lines_set()).unwrap();
        let saved: FocusMode = toml::from_str(&text).unwrap();
        assert_eq!(saved.preview_line_count(), 1, "{text}");
    }

    #[test]
    fn the_switches_it_replaced_are_never_written() {
        let text = toml::to_string(&FocusMode::default()).unwrap();
        assert!(!text.contains("hide_preview"), "{text}");
        assert!(!text.contains("one_preview_line"), "{text}");
    }

    #[test]
    fn focus_settings_read_as_the_sidebar_wants_them() {
        let mut focus = FocusMode::default();
        assert_eq!(
            FocusMode::section_placement(true),
            SectionPlacement::AboveAccounts
        );
        assert_eq!(FocusMode::section_placement(false), SectionPlacement::Off);
        assert_eq!(focus.contacts_placement(), SectionPlacement::Footer);
        focus.show_contacts = false;
        assert_eq!(focus.contacts_placement(), SectionPlacement::Off);
        assert!(focus.rail_fold().folds_accounts());
        focus.fold_accounts = false;
        assert!(!focus.rail_fold().folds_accounts());
        focus.unread_counts = false;
        assert!(!focus.unified_chips().starred);
    }

    #[test]
    fn focus_round_trips_and_fills_missing_parts() {
        let mut f = FocusMode::default();
        f.set(FocusPart::Enabled, true);
        f.set(FocusPart::HideAvatars, false);
        let text = toml::to_string(&f).unwrap();
        let back: FocusMode = toml::from_str(&text).unwrap();
        assert_eq!(back, f);
        assert!(back.active(FocusPart::ReaderToolbar));
        assert!(!back.active(FocusPart::HideAvatars));
        // A file from before a part existed reads it as ticked — the
        // preview and rail parts included, so an upgrade gets the whole
        // mode without anyone going looking for the new switches.
        let partial: FocusMode = toml::from_str("enabled = true").unwrap();
        assert!(partial.enabled && partial.fold_unified);
        assert!(partial.hide_preview && partial.rail_sidebar);
        // Except the subject and the row's marks, off wherever they come from.
        assert!(!partial.hide_subject);
        assert!(!partial.hide_stars && !partial.hide_attachment_icons);
    }
}
