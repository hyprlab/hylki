//! The settings window's messages: what it starts with, what the app and
//! its own rows send it, and the preference changes it reports.

use super::*;

/// Initial data for the settings window.
#[derive(Debug)]
pub(crate) struct PrefInit {
    pub(crate) auto_remote_content: bool,
    pub(crate) show_remote_banner: bool,
    pub(crate) show_spoof_banner: bool,
    pub(crate) gravatar: bool,
    pub(crate) avatars: bool,
    /// Your own mail wears its mailbox's face, not a sender's circle (#189).
    pub(crate) own_mailbox_face: bool,
    /// Tagged rows are washed with their tag's color (PR #383).
    pub(crate) tag_row_tint: bool,
    pub(crate) sender_logos: bool,
    pub(crate) date_style: DateStyle,
    pub(crate) clock_style: ClockStyle,
    /// The chosen interface language code; empty = the system's (#179).
    pub(crate) language: String,
    pub(crate) fetch_interval_secs: u64,
    pub(crate) push: bool,
    pub(crate) metered_save_data: bool,
    pub(crate) palette_collapse_secs: u64,
    /// Seconds a message card's actions palette stays open — its own, not
    /// the message list's.
    pub(crate) card_palette_collapse_secs: u64,
    pub(crate) threading: bool,
    pub(crate) threads_expanded: bool,
    /// Reading pane shows conversations newest-message-first.
    pub(crate) thread_newest_first: bool,
    /// Which messages of a conversation open folded (#326).
    pub(crate) fold_messages: crate::config::FoldMessages,
    /// Reader always shows the recipients line under the sender.
    pub(crate) always_show_recipients: bool,
    /// The OpenPGP chip says its verdict in words (#300).
    pub(crate) pgp_labels: bool,
    /// The reader's find bar opens under the message (#392).
    pub(crate) reader_find_bottom: bool,
    /// Lone messages render as inset cards, like conversation messages.
    pub(crate) single_message_card: bool,
    /// The Light / Dark Mode switch is shown in the reader header (PR #386).
    pub(crate) theme_switch: bool,
    /// Icons come from the desktop's icon theme, not Hylki's own.
    pub(crate) desktop_icons: bool,
    /// The Reader View switch is shown in the reader header.
    pub(crate) reader_switch: bool,
    /// What Reader View does when a message is opened.
    pub(crate) reader_default: crate::config::ReaderDefault,
    /// The message zoom every launch starts at, in percent.
    pub(crate) reader_zoom: u32,
    /// Each conversation message lists its own attachments (#213).
    pub(crate) card_attachments: bool,
    /// The attachment drawer beneath the reader is shown (#213).
    pub(crate) attachment_drawer: bool,
    /// Conversation rows may expand into their members in the message list.
    pub(crate) thread_expansion: bool,
    /// A conversation's row speaks for the newest message anywhere in the
    /// account, the replies you sent included (#236).
    pub(crate) thread_row_newest: bool,
    /// Deleting a whole selected conversation asks for confirmation.
    pub(crate) confirm_thread_delete: bool,
    /// Conversation card actions hide until the card is hovered.
    pub(crate) card_actions_hover: bool,
    /// With the ⋯ toggle off: card actions appear automatically on hover.
    pub(crate) card_actions_auto: bool,
    /// The list rows carry an actions palette line at all.
    pub(crate) list_palette: bool,
    /// The list's actions palette opens on row hover (no ⋯ click).
    pub(crate) list_palette_hover: bool,
    /// A message card's ⋯ opens the card menu instead of sliding its
    /// actions palette out.
    pub(crate) card_palette_menu: bool,
    /// Message rows take a sideways swipe to archive / delete (#92).
    pub(crate) swipe_enabled: bool,
    /// The message list's swipe-gesture sides are swapped (#swipe).
    pub(crate) swipe_reversed: bool,
    /// How far a trackpad two-finger swipe has to travel to fire the action.
    pub(crate) swipe_sensitivity: f64,
    /// "New message" composes inline over the reading pane (vs a window).
    pub(crate) compose_inline: bool,
    /// Reply, Reply All and Forward open in the reading pane (vs a window).
    pub(crate) reply_inline: bool,
    pub(crate) reply_fields: bool,
    /// Settings → System → GNOME Files: what handed-in files open into.
    pub(crate) files: crate::config::FilesPrefs,
    /// Settings → System → Links: the browser links open in (#232). Empty =
    /// the desktop's default, `ask` = its app chooser, otherwise a desktop
    /// entry id.
    pub(crate) link_browser: String,
    /// The identity new messages are sent from (#157); empty = the open
    /// folder's account. One of `identities`' addresses.
    pub(crate) compose_default_from: String,
    pub(crate) paste_plain: bool,
    pub(crate) return_paragraph: bool,
    pub(crate) toolbar_expanded: bool,
    /// The font rich-text messages are written in, as a CSS family list,
    /// and its size in points; empty and 0 are the defaults.
    pub(crate) compose_font: String,
    pub(crate) compose_font_size: u32,
    /// Settings → Composing → Default zoom; 0 = the reader's.
    pub(crate) compose_zoom: u32,
    /// Settings → Reading → Printing (#359).
    pub(crate) print_options: crate::config::PrintOptions,
    pub(crate) spellcheck: bool,
    pub(crate) spellcheck_langs: String,
    pub(crate) message_theme: MessageTheme,
    /// Set every message in the reader's own font (#56).
    pub(crate) override_fonts: bool,
    /// That font, as a Pango description; empty = the interface font.
    pub(crate) reader_font: String,
    /// Ignore the senders' text and background colors (#56).
    pub(crate) override_colors: bool,
    /// Plain-text messages in monospace (#181), and the font ("" = the
    /// desktop's monospace font).
    pub(crate) plain_monospace: bool,
    pub(crate) plain_font: String,
    /// New messages start as plain text (#180).
    pub(crate) compose_format: crate::config::ComposeFormat,
    /// Replies and forwards to plain-text mail are written in plain text (#436).
    pub(crate) follow_plain: bool,
    /// Where the split reply opens in the reading pane (#212).
    pub(crate) reply_position: crate::config::ReplyPosition,
    /// Where the signature sits in a reply or forward (#237).
    pub(crate) signature_position: crate::config::SignaturePosition,
    pub(crate) signature_dashes: bool,
    /// The line above a reply's quote, "" for the default (#528).
    pub(crate) reply_header: String,
    /// Undo Send delay in seconds, 0 for off (#434).
    pub(crate) undo_send_secs: u32,
    pub(crate) app_theme: AppTheme,
    pub(crate) text_scale: u32,
    /// The appearance theme's id ("system" for the stock GNOME colors).
    pub(crate) theme: String,
    pub(crate) notifications: bool,
    pub(crate) notification_content: bool,
    pub(crate) notification_buttons: crate::config::NotificationButtons,
    pub(crate) show_attachments: bool,
    pub(crate) show_unified: bool,
    pub(crate) unified_chips: crate::config::UnifiedChips,
    pub(crate) unified_kinds: crate::config::UnifiedKinds,
    pub(crate) show_accounts: bool,
    pub(crate) section_style: crate::config::SectionStyle,
    pub(crate) filtered_placement: crate::config::SectionPlacement,
    pub(crate) tags_placement: crate::config::SectionPlacement,
    pub(crate) favorites_placement: crate::config::SectionPlacement,
    pub(crate) contacts_placement: crate::config::SectionPlacement,
    pub(crate) chevrons_left: bool,
    pub(crate) account_click_folds: bool,
    pub(crate) start_view: crate::config::StartView,
    pub(crate) folder_sort: crate::config::FolderSort,
    pub(crate) console_mode: bool,
    pub(crate) status_bar_top: bool,
    pub(crate) read_mark: crate::config::ReadMark,
    pub(crate) sidebar_hover_expand: bool,
    pub(crate) remember_sidebar: bool,
    pub(crate) remember_rail: bool,
    pub(crate) rail_dots: bool,
    pub(crate) seen_counts: bool,
    pub(crate) rail_fold: crate::config::RailFold,
    /// The reader header's buttons, per side and in order.
    pub(crate) reader_toolbar: ReaderToolbar,
    pub(crate) toolbar_fold_right: bool,
    /// Focus Mode: the master switch and the parts it strips.
    pub(crate) focus: crate::config::FocusMode,
    pub(crate) preview_lines: u32,
    pub(crate) list_layout: crate::config::ListLayout,
    pub(crate) list_columns: Vec<crate::config::ListColumn>,
    pub(crate) list_headings: bool,
    pub(crate) list_column_scroll: bool,
    pub(crate) list_group_by_date: bool,
    pub(crate) list_pills_below: bool,
    pub(crate) list_search_open: bool,
    pub(crate) contacts_search_open: bool,
    pub(crate) list_search_bottom: bool,
    pub(crate) list_bulk_bar_bottom: bool,
    pub(crate) single_key_shortcuts: bool,
    pub(crate) gmail_keys: bool,
    pub(crate) keep_downloads_days: u32,
    pub(crate) run_in_background: bool,
    pub(crate) autostart: bool,
    pub(crate) start_fullscreen: bool,
    pub(crate) tray: bool,
    pub(crate) tray_icon: TrayIcon,
    pub(crate) tray_mail: bool,
    pub(crate) launcher_badge: LauncherBadge,
    /// The chosen app icon (an `app_icon::catalog` id).
    pub(crate) app_icon: String,
    /// The accounts panel (built by the AccountsWindow component), shown
    /// behind the window's "Accounts" tab.
    pub(crate) accounts_panel: gtk::Widget,
    /// Open showing the Accounts tab instead of Preferences.
    pub(crate) start_on_accounts: bool,
    /// The category to open on, when the app remembers one from earlier
    /// this session; overrides `start_on_accounts`.
    pub(crate) start_page: Option<String>,
    /// The persisted "this window opens to" choice (true = Accounts).
    pub(crate) settings_open_accounts: bool,
    /// The accounts component's inbox, for the sidebar to pick its pages.
    pub(crate) accounts_sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    /// (name, address) per account and alias, for the OpenPGP page's
    /// key generator.
    pub(crate) identities: Vec<(String, String)>,
}

#[derive(Debug)]
pub(crate) enum PrefInput {
    /// The Contacts page: the books new contacts can go to, freshly read.
    SetContactBooks(Vec<crate::contacts::Book>),
    /// The Hylki book's size changed (an import, a deletion).
    SetLocalContactCount(usize),
    ToggleAvatars(bool),
    ChangeDateStyle(u32),
    ChangeClockStyle(u32),
    ChangeLanguage(u32),
    ChangeRenderer(u32),
    /// The GNOME Files extension (#188): install this build's copy, remove
    /// the installed one, ask Files to quit so it reloads.
    NautilusInstall,
    NautilusRemove,
    NautilusRestartFiles,
    /// The GNOME Files hand-off rows: destination, over-the-limit action,
    /// and the limit in MB.
    ChangeFilesAction(u32),
    ChangeFilesLarge(u32),
    ChangeFilesLimit(u32),
    /// Settings → System → Links: the browser row moved (#232).
    ChangeLinkBrowser(u32),
    /// The app changed the Files preferences (a dialog's "always do this").
    SetFilesPrefs(crate::config::FilesPrefs),
    /// The spelling languages changed, possibly from a composer (#365).
    SetSpellcheckLangs(String),
    /// Re-read the extension's state (the System page came into view; Files
    /// may have loaded the extension since).
    NautilusRefresh,
    /// Settings → System → Storage (#394): measure what downloaded mail
    /// takes up, off the main thread, and the answer.
    MeasureStorage,
    StorageMeasured {
        bodies: u64,
        files: u64,
        total: u64,
    },
    RemoveDownloads,
    RemoveDownloadsNow,
    ChangeKeepDownloads(u32),
    ToggleThreading(bool),
    ChangeReaderDefault(u32),
    ChangeReaderZoom(u32),
    ToggleThreadExpansion(bool),
    ChangeCardActionsMode(u32),
    ToggleListPalette(bool),
    ToggleSwipeEnabled(bool),
    /// The "Send new messages from" combo: 0 = the open folder's account,
    /// then `identities` in order.
    ChangeComposeDefaultFrom(u32),
    ChangeFetchInterval(u32),
    ToggleNotifications(bool),
    ToggleNotificationButton(crate::config::NotificationButton, bool),
    /// The new-mail sound (#292): on or off, which one (an index into the
    /// built-ins, then Custom File), pick a file, it was picked, hear it.
    ToggleSound(bool),
    ToggleSoundFocused(bool),
    ChangeSound(u32),
    ChooseSound,
    SoundChosen(std::path::PathBuf),
    PlaySound,
    ToggleShowUnified(bool),
    ToggleUnifiedChipAllInboxes(bool),
    TogglePrintPageNumbers(bool),
    TogglePrintDate(bool),
    TogglePrintPunchMargin(bool),
    ToggleUnifiedChipStarred(bool),
    ToggleUnifiedChipDrafts(bool),
    ToggleUnifiedChipArchive(bool),
    ToggleUnifiedChipJunk(bool),
    ToggleUnifiedChipTrash(bool),
    ToggleUnifiedStarred(bool),
    ToggleUnifiedSent(bool),
    ToggleUnifiedDrafts(bool),
    ToggleUnifiedArchive(bool),
    ToggleUnifiedJunk(bool),
    ToggleUnifiedTrash(bool),
    ToggleShowAccounts(bool),
    /// One of Focus Mode's switches.
    ToggleFocus(crate::config::FocusPart, bool),
    /// The app's Focus Mode changed elsewhere (menu, shortcut): follow.
    SetFocusMode(crate::config::FocusMode),
    /// The main menu flipped "Show Accounts": the switch follows.
    SetShowAccounts(bool),
    ChangeChevronSide(u32),
    ChangeStartView(u32),
    ChangeFolderSort(u32),
    ChangeSectionStyle(u32),
    ChangeFilteredPlacement(u32),
    ChangeTagsPlacement(u32),
    ChangeFavoritesPlacement(u32),
    ChangeContactsPlacement(u32),
    /// A reader toolbar chip was dropped: `key` names the button, `side`
    /// the zone (None = not shown), `index` its place in that zone.
    ToolbarDrop {
        key: String,
        side: Option<ToolbarSide>,
        index: usize,
    },
    /// A column chip dropped among the shown columns at `index`, or among
    /// the hidden ones (#334).
    ColumnDrop {
        key: String,
        shown: bool,
        index: usize,
    },
    ColumnRestore,
    ToolbarRestore,
    /// The showcase's stand-in for a drag hovering a zone: open the gap at
    /// `index` of the zone (0 left, 1 right, 2 not shown).
    ToolbarGapPreview {
        zone: usize,
        index: usize,
    },
    ToggleRailFoldEnabled(bool),
    ToggleRailFoldAccounts(bool),
    ToggleRailFoldAllInboxes(bool),
    ToggleRailFoldStarred(bool),
    ToggleRailFoldSent(bool),
    ToggleRailFoldDrafts(bool),
    ToggleRailFoldArchive(bool),
    ToggleRailFoldJunk(bool),
    ToggleRailFoldTrash(bool),
    ToggleRailFoldFiltered(bool),
    ToggleRailFoldTags(bool),
    ChangePreviewLines(u32),
    ChangeListLayout(u32),
    ChangeReadMark(u32),
    ExportSettings,
    ExportLog,
    ImportSettings,
    ChangeTrayIcon(u32),
    ChangeLauncherBadge(u32),
    ChangeMessageTheme(u32),
    ChangeComposeFormat(u32),
    ChangeReplyPosition(u32),
    ChangeFoldMessages(u32),
    ChangeSignaturePosition(u32),
    ChangeUndoSend(u32),
    ChangeAppTheme(u32),
    ChangeTextScale(u32),
    ChangeSettingsOpen(u32),
    /// Switch the window to the Accounts panel (true) or Preferences (false).
    ShowAccounts(bool),
    /// Put the deferred pages back into the stack (scheduled after the
    /// first paint).
    MountPages,
    /// The settings search (#260): the text typed, and a result picked.
    Search(String),
    SearchPick(usize),
    /// Ctrl+F: open the search, or close it when open.
    ToggleSearch,
    /// A fresh accounts panel for this open (the window is kept between
    /// opens; the panel is rebuilt over the current accounts).
    SetAccountsPanel {
        panel: gtk::Widget,
        sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    },
    /// A sidebar category was chosen (#141).
    SelectPage(String),
    /// The open editor says it holds unsaved changes: ask before leaving it
    /// for this page.
    LeaveEditorPrompt(String),
    /// Leave the editor for this page — it is saved, discarded, or was never
    /// touched.
    LeaveEditorTo(String),
    /// Select a category by id from outside (the app's showcase hook).
    ShowPageById(String),
    /// Message List, scrolled to its Columns group (#443).
    RevealColumns,
    /// An accounts-panel editor subpage (account, filter or tag) opened on
    /// the named settings page, or closed — hide/show the shared header so
    /// the editor's own header takes over the window.
    EditorOpen(Option<&'static str>),
    /// The Cloud Storage page's account editor is up (or gone).
    CloudEditorOpen(bool),
    /// What the Contacts page asks of the window and the app.
    Contacts(crate::ui::contact_sources::ContactSourcesOutput),
}

#[derive(Debug)]
pub(crate) enum PrefOutput {
    /// The Contacts page's buttons: the app opens the file dialogs and the
    /// confirmation, and does the work off the main thread.
    ImportLocalContacts,
    ExportLocalContacts,
    DeleteAllLocalContacts,
    SetContactBook(String),
    /// The Contacts page is showing: read the books new contacts can go to
    /// (D-Bus, so off the main thread) and send them back.
    LoadContactBooks,
    /// An address book was added, changed, removed or switched on or off
    /// for Hylki (#408): sync the books and read the contacts again.
    ContactSourcesChanged,
    /// A category was shown, so the app can reopen the window on it.
    PageShown(String),
    SetAutoRemoteContent(bool),
    SetShowRemoteBanner(bool),
    SetShowSpoofBanner(bool),
    SetGravatar(bool),
    SetAvatars(bool),
    SetOwnMailboxFace(bool),
    SetTagRowTint(bool),
    SetSenderLogos(bool),
    SetDateStyle(DateStyle),
    SetClockStyle(ClockStyle),
    /// The interface language code chosen, "" for the system's (#179).
    SetLanguage(String),
    /// A `GSK_RENDERER` value, or empty for GTK's own choice (#403).
    SetRenderer(String),
    SetThreading(bool),
    SetThreadsExpanded(bool),
    SetThreadNewestFirst(bool),
    SetFoldMessages(crate::config::FoldMessages),
    SetAlwaysShowRecipients(bool),
    SetPgpLabels(bool),
    SetReaderFindBottom(bool),
    SetSingleMessageCard(bool),
    SetReaderSwitch(bool),
    SetThemeSwitch(bool),
    SetDesktopIcons(bool),
    SetReaderDefault(crate::config::ReaderDefault),
    SetReaderZoom(u32),
    SetCardAttachments(bool),
    SetAttachmentDrawer(bool),
    SetThreadExpansion(bool),
    SetThreadRowNewest(bool),
    SetConfirmThreadDelete(bool),
    SetCardActionsMode {
        hover_toggle: bool,
        hover_auto: bool,
    },
    SetListPalette(bool),
    SetListPaletteHover(bool),
    SetCardPaletteMenu(bool),
    SetSwipeEnabled(bool),
    SetSwipeReversed(bool),
    SetSwipeSensitivity(f64),
    SetComposeInline(bool),
    SetReplyInline(bool),
    SetReplyFields(bool),
    SetFilesPrefs(crate::config::FilesPrefs),
    /// The browser links open in (#232): "" = the desktop's default,
    /// "ask" = its app chooser, otherwise a desktop entry id.
    SetLinkBrowser(String),
    SetComposeDefaultFrom(String),
    SetPastePlain(bool),
    SetReturnParagraph(bool),
    SetToolbarExpanded(bool),
    SetComposeFont(String),
    SetComposeFontSize(u32),
    SetComposeZoom(u32),
    SetSpellcheck(bool),
    SetSpellcheckLangs(String),
    SetFetchInterval(u64),
    SetPush(bool),
    SetMeteredSaveData(bool),
    SetNotifications(bool),
    SetNotificationContent(bool),
    SetNotificationButtons(crate::config::NotificationButtons),
    SetAttachmentsRow(bool),
    SetShowUnified(bool),
    SetUnifiedChips(crate::config::UnifiedChips),
    SetPrintOptions(crate::config::PrintOptions),
    SetUnifiedKinds(crate::config::UnifiedKinds),
    SetShowAccounts(bool),
    SetChevronsLeft(bool),
    SetAccountClickFolds(bool),
    SetStartView(crate::config::StartView),
    SetFolderSort(crate::config::FolderSort),
    SetSectionStyle(crate::config::SectionStyle),
    SetFilteredPlacement(crate::config::SectionPlacement),
    SetTagsPlacement(crate::config::SectionPlacement),
    SetFavoritesPlacement(crate::config::SectionPlacement),
    SetContactsPlacement(crate::config::SectionPlacement),
    SetConsoleMode(bool),
    SetStatusBarTop(bool),
    SetReadMark(crate::config::ReadMark),
    ExportSettings,
    ExportLog,
    ImportSettings,
    SetSidebarHoverExpand(bool),
    SetRememberSidebar(bool),
    SetRememberRail(bool),
    SetRailDots(bool),
    SetSeenCounts(bool),
    SetReaderToolbar(ReaderToolbar),
    SetToolbarFoldRight(bool),
    SetFocusMode(crate::config::FocusMode),
    SetRailFold(crate::config::RailFold),
    SetAppTheme(AppTheme),
    SetTextScale(u32),
    /// A theme was picked in the gallery (its id, or "system").
    SetTheme(String),
    /// The "this window opens to" choice changed (true = Accounts).
    SetSettingsOpenAccounts(bool),
    SetPreviewLines(u32),
    SetListLayout(crate::config::ListLayout),
    /// The single-line list's columns, in order (#334).
    SetListColumns(Vec<crate::config::ListColumn>),
    SetListHeadings(bool),
    /// Scroll a single line too wide for the pane, or fit its columns (#443).
    SetListColumnScroll(bool),
    /// Date headings over a list sorted by date (#435).
    SetGroupByDate(bool),
    /// A card's tags and labels on a line under its text (#374).
    SetPillsBelow(bool),
    /// Keep the list's search field open (#446).
    SetListSearchOpen(bool),
    /// Keep Contacts' search field open.
    SetContactsSearchOpen(bool),
    /// The list's search field under the rows, not above them (#496).
    SetListSearchBottom(bool),
    /// The list's bulk bar under the rows, not above them (#496).
    SetListBulkBarBottom(bool),
    /// Every column back to its own width (#334).
    ResetColumnWidths,
    SetSingleKey(bool),
    SetStartFullscreen(bool),
    SetGmailKeys(bool),
    /// How many days downloaded mail is kept; 0 keeps it (#394).
    SetKeepDownloads(u32),
    SetRunInBackground(bool),
    SetAutostart(bool),
    SetTray(bool),
    SetTrayIcon(TrayIcon),
    SetTrayMail(bool),
    SetLauncherBadge(LauncherBadge),
    SetAppIcon(String),
    SetPaletteCollapse(u64),
    SetCardPaletteCollapse(u64),
    SetMessageTheme(MessageTheme),
    SetOverrideFonts(bool),
    SetReaderFont(String),
    SetOverrideColors(bool),
    SetPlainMonospace(bool),
    SetPlainFont(String),
    SetComposeFormat(crate::config::ComposeFormat),
    SetFollowPlain(bool),
    SetReplyPosition(crate::config::ReplyPosition),
    SetSignaturePosition(crate::config::SignaturePosition),
    SetSignatureDashes(bool),
    SetReplyHeader(String),
    SetUndoSend(u32),
    Closed,
}
