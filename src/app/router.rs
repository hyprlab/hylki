//! The root component's `update`: one match that hands each message to
//! the module that handles it. The arms whose order matters (the busy
//! pointer gate before the compose messages, the showcase guard before
//! the delete-attachment question, and the catch-all for settings) are
//! kept here.

use adw::prelude::*;
use relm4::prelude::*;

use crate::ui::preferences::PrefOutput;

use super::model::AppModel;
use super::msg::AppMsg;

impl AppModel {
    /// Hand `msg` to the module that handles it.
    pub(super) fn route(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            // Building a composer holds the window a moment, longest the
            // first time in a session: the pointer turns busy, and that is
            // drawn before the build starts, so the key or click is seen to
            // have landed (#392). Another press meanwhile is dropped.
            asked @ (AppMsg::Compose
            | AppMsg::Reply
            | AppMsg::ReplyAll
            | AppMsg::Forward
            | AppMsg::ComposeInWindow(_))
                if !self.compose_now =>
            {
                if !std::mem::replace(&mut self.compose_waiting, true) {
                    self.window.set_cursor_from_name(Some("progress"));
                    let s = sender.clone();
                    self.after_paint(move || s.input(AppMsg::ComposeNow(Box::new(asked))));
                }
            }
            AppMsg::ComposeNow(asked) => {
                self.compose_waiting = false;
                self.compose_now = true;
                <Self as SimpleComponent>::update(self, *asked, sender.clone());
                self.compose_now = false;
                // Back once the composer's first frame, the slow one, is drawn.
                let window = self.window.clone();
                self.after_paint(move || window.set_cursor(None::<&gtk::gdk::Cursor>));
            }
            AppMsg::AskDeleteAttachment(target)
                if std::mem::take(&mut self.showcase_confirm_delete) =>
            {
                sender.input(AppMsg::DeleteAttachment(target));
            }
            msg @ (AppMsg::Snooze(..)
            | AppMsg::Unsnooze
            | AppMsg::PickSnoozeTime
            | AppMsg::SnoozeMessages { .. }
            | AppMsg::SnoozeMenu
            | AppMsg::WakeSnoozed
            | AppMsg::Unsnoozed { .. }
            | AppMsg::SnoozedCount { .. }) => self.on_snooze(msg, sender),
            msg @ (AppMsg::ShowOutbox
            | AppMsg::OutboxItems { .. }
            | AppMsg::EditCurrentOutbox
            | AppMsg::SendCurrentOutbox
            | AppMsg::SendDueScheduled
            | AppMsg::RetryAllOutbox) => self.on_outbox(msg, sender),
            msg @ (AppMsg::Notice(..)
            | AppMsg::OpenMessageFromNotification { .. }
            | AppMsg::NotificationMarkRead { .. }
            | AppMsg::NotificationArchive { .. }
            | AppMsg::NotificationDelete { .. }
            | AppMsg::NotificationSpam { .. }
            | AppMsg::NotificationReply { .. }
            | AppMsg::NotificationForward { .. }
            | AppMsg::Status { .. }
            | AppMsg::Error { .. }
            | AppMsg::DirectoryError(..)
            | AppMsg::NotifyCount(..)
            | AppMsg::ToggleNotifications
            | AppMsg::StatusBarShown(..)) => self.on_notifications(msg, sender),
            msg @ (AppMsg::ShowAttachments
            | AppMsg::GalleryFetch { .. }
            | AppMsg::AttachmentsScanned { .. }
            | AppMsg::GalleryLoad(..)
            | AppMsg::OpenAttachmentMessage { .. }) => self.on_gallery(msg),
            msg @ (AppMsg::UnifiedSelected(..)
            | AppMsg::AccountStarredSelected(..)
            | AppMsg::FolderSelected { .. }) => self.on_navigation(msg, sender),
            msg @ (AppMsg::AccountFolded { .. }
            | AppMsg::ToggleCustomFolders(..)
            | AppMsg::ReorderFolders { .. }
            | AppMsg::PlaceFolder { .. }
            | AppMsg::SetAccountFolderSort { .. }
            | AppMsg::FolderNodeCollapsed { .. }
            | AppMsg::SidebarSectionsOpen { .. }
            | AppMsg::AccountsReordered(..)
            | AppMsg::ToggleAccountTags(..)) => self.on_sidebar(msg),
            msg @ (AppMsg::SidebarCollapsed(..)
            | AppMsg::AutoRail(..)
            | AppMsg::ToggleSidebar
            | AppMsg::SidebarPeekDismissed
            | AppMsg::SidebarHoverEnter) => self.on_sidebar_peek(msg),
            msg @ (AppMsg::SidebarContext(..)
            | AppMsg::DropMoveMessages { .. }
            | AppMsg::CreateFolder { .. }
            | AppMsg::MoveFolder { .. }
            | AppMsg::RenameFolderTo { .. }
            | AppMsg::EmptyFolder { .. }
            | AppMsg::DeleteFolder { .. }) => self.on_folder_menu(msg, sender),
            msg @ (AppMsg::SearchPoolLoaded { .. }
            | AppMsg::SearchPoolBuilt { .. }
            | AppMsg::SearchActive(..)) => self.on_search(msg, sender),
            msg @ (AppMsg::FindInBodies { .. }
            | AppMsg::BodiesFound { .. }
            | AppMsg::FindOnServers { .. }
            | AppMsg::TextFound { .. }) => self.on_text_search(msg, sender),
            msg @ (AppMsg::OpenedByKey { .. }
            | AppMsg::ClearReader
            | AppMsg::MessageSelected { .. }
            | AppMsg::ThreadGrew { .. }
            | AppMsg::Related { .. }
            | AppMsg::ThreadsListed { .. }
            | AppMsg::ThreadSummaries { .. }
            | AppMsg::OpenListSearch
            | AppMsg::OpenReaderFind
            | AppMsg::SetQuickFilter(..)
            | AppMsg::SetFilterDate(..)
            | AppMsg::ClearFilterDates
            | AppMsg::ClearQuickFilters
            | AppMsg::DeferredMarkRead { .. }
            | AppMsg::SelectionKeys(..)
            | AppMsg::ListChecked(..)
            | AppMsg::SelectCards(..)
            | AppMsg::ThreadMessageSeen { .. }
            | AppMsg::RenderThread) => self.on_reader_select(msg, sender),
            msg @ (AppMsg::OpenMessageWindow { .. }
            | AppMsg::PopoutClosed(..)
            | AppMsg::AddContactFrom { .. }
            | AppMsg::PopoutMoveTo { .. }
            | AppMsg::PopoutShortcut { .. }
            | AppMsg::ViewSource) => self.on_popout(msg, sender),
            msg @ (AppMsg::LoadAttachmentsFor(..)
            | AppMsg::DeletePopoutAttachment { .. }
            | AppMsg::ShowLightbox { .. }
            | AppMsg::CardAttachment { .. }
            | AppMsg::ShowAttachmentInMessage(..)
            | AppMsg::DeleteDrawerAttachment(..)
            | AppMsg::AskDeleteAttachment(..)
            | AppMsg::DeleteAttachment(..)
            | AppMsg::SendDeleteAttachment(..)
            | AppMsg::AttachmentNotDeleted { .. }
            | AppMsg::AttachmentDeleted { .. }) => self.on_attachments(msg, sender),
            msg @ (AppMsg::ShowcasePopout
            | AppMsg::ShowcaseFolder { .. }
            | AppMsg::ShowcaseNotify
            | AppMsg::ShowcaseDeleteAttachment { .. }
            | AppMsg::ShowcaseToolbarGap { .. }
            | AppMsg::ShowcaseComposeMenu { .. }
            | AppMsg::ShowcaseComposePreview
            | AppMsg::ShowcaseComposeFrom(..)
            | AppMsg::ShowcaseForward { .. }
            | AppMsg::ShowcaseComposeUndo
            | AppMsg::ShowcaseBurger
            | AppMsg::ShowcaseComposeFormat(..)
            | AppMsg::ShowcaseComposeClose
            | AppMsg::ShowcaseCopy
            | AppMsg::ShowcaseSend(..)
            | AppMsg::ShowcaseNewAccount(..)
            | AppMsg::ShowcaseDirtyEditor
            | AppMsg::ShowcaseEditFilter(..)) => self.on_showcase(msg, sender),
            msg @ (AppMsg::ToggleStar
            | AppMsg::Archive
            | AppMsg::Delete
            | AppMsg::DeleteForever
            | AppMsg::PurgeMessages(..)
            | AppMsg::CardAction { .. }
            | AppMsg::ToggleReadCurrent
            | AppMsg::CardContact(..)
            | AppMsg::RowAction { .. }
            | AppMsg::Bulk { .. }
            | AppMsg::BulkComplete
            | AppMsg::MarkSpam
            | AppMsg::DeleteThread(..)
            | AppMsg::DeleteThreadConfirmed(..)
            | AppMsg::DeleteFromAddress { .. }) => self.on_actions(msg, sender),
            msg @ (AppMsg::Refresh
            | AppMsg::SystemResumed
            | AppMsg::NetworkBack
            | AppMsg::NetworkLost
            | AppMsg::MeteredChanged
            | AppMsg::LauncherCountLost
            | AppMsg::WindowInFront) => self.on_workers(msg, sender),
            msg @ (AppMsg::Compose
            | AppMsg::Reply
            | AppMsg::ReplyAll
            | AppMsg::Forward
            | AppMsg::ComposeInWindow(..)
            | AppMsg::EditAsNewCurrent
            | AppMsg::ComposeTo(..)
            | AppMsg::ComposeBcc(..)
            | AppMsg::OpenMailto(..)
            | AppMsg::PresentComposers) => self.on_compose_open(msg, sender),
            msg @ (AppMsg::AddContactAddr(..)
            | AppMsg::AddContactBooks { .. }
            | AppMsg::ContactAdded(..)
            | AppMsg::ContactPhotosChanged
            | AppMsg::ShowContactsPage
            | AppMsg::OpenContacts
            | AppMsg::ContactTree(..)
            | AppMsg::ContactScope(..)
            | AppMsg::ContactBooksSynced
            | AppMsg::ContactBooksLoaded { .. }
            | AppMsg::ContactSuggestions
            | AppMsg::ContactsLoaded(..)
            | AppMsg::ContactNamesRead(..)
            | AppMsg::LaunchGnomeContacts
            | AppMsg::SaveContact { .. }
            | AppMsg::CreateContact { .. }
            | AppMsg::SaveGroup { .. }
            | AppMsg::DeleteContacts(..)
            | AppMsg::MoveGroup { .. }
            | AppMsg::ContactDrop { .. }
            | AppMsg::ContactTreeAction(..)
            | AppMsg::ContactsFolded(..)
            | AppMsg::RenameBook { .. }
            | AppMsg::CopyContacts { .. }
            | AppMsg::MoveContacts { .. }
            | AppMsg::ContactWriteDone(..)
            | AppMsg::ImportContacts(..)
            | AppMsg::ContactsImported(..)) => self.on_contacts(msg, sender),
            msg @ AppMsg::OpenAbout => self.on_about(msg, sender),
            msg @ (AppMsg::AllowSender(..)
            | AppMsg::AddSender(..)
            | AppMsg::RemoveSender(..)
            | AppMsg::AddBlacklist(..)
            | AppMsg::RemoveBlacklist(..)
            | AppMsg::SetFilters(..)) => self.on_filters(msg),
            msg @ (AppMsg::Unsubscribe { .. }
            | AppMsg::UnsubscribeGo { .. }
            | AppMsg::UnsubscribeByMail { .. }
            | AppMsg::UnsubscribeDone { .. }) => self.on_unsubscribe(msg, sender),
            msg @ AppMsg::InviteAction { .. } => self.on_invites(msg),
            AppMsg::ReceiptAnswer { message, send } => self.answer_receipt(&message, send),
            msg @ (AppMsg::ListOverflowMenu
            | AppMsg::ListPaneWidth(..)
            | AppMsg::ListCount(..)
            | AppMsg::ListColumnWidths(..)
            | AppMsg::ListSortChanged(..)) => self.on_list_header(msg, sender),
            msg @ AppMsg::OwnGravatarFetched { .. } => self.on_identity(msg),
            msg @ (AppMsg::SetReaderActionsCollapsed(..)
            | AppMsg::SetReaderCompact(..)
            | AppMsg::MeasureReaderFold
            | AppMsg::ReaderControlsChanged(..)
            | AppMsg::ReaderOverflowMenu
            | AppMsg::ReaderToolbarMenu { .. }
            | AppMsg::CustomizeToolbar
            | AppMsg::CustomizeColumns) => self.on_reader_toolbar(msg, sender),
            msg @ (AppMsg::WizardLanguage(..)
            | AppMsg::RestartApp
            | AppMsg::RestartNow
            | AppMsg::QuitFromTray
            | AppMsg::TrayViewUnread
            | AppMsg::PresentWindow) => self.on_tray(msg, sender),
            msg @ (AppMsg::ShowShortcuts
            | AppMsg::CommandPalette(..)
            | AppMsg::PrintPreview
            | AppMsg::PrintMessage
            | AppMsg::Shortcut(..)
            | AppMsg::ZoomKey(..)
            | AppMsg::ComposerKey(..)
            | AppMsg::ZoomMessage(..)
            | AppMsg::CopyReaderSelection) => self.on_shortcuts(msg, sender),
            msg @ (AppMsg::SetZoomDefault(..)
            | AppMsg::SetReaderMode(..)
            | AppMsg::SetReaderSwitchShown(..)
            | AppMsg::SetThemeSwitchShown(..)
            | AppMsg::SetRemoteContent { .. }
            | AppMsg::ReloadBody(..)) => self.on_prefs_reader(msg),
            msg @ (AppMsg::Undo | AppMsg::Redo | AppMsg::UndoRestored { .. }) => self.on_undo(msg),
            msg @ AppMsg::ComposeHistory { .. } => self.on_prefs_compose(msg),
            msg @ (AppMsg::HandOffAction { .. }
            | AppMsg::HandOffDraftPickNow
            | AppMsg::HandOffDraft { .. }
            | AppMsg::HandOffReply { .. }
            | AppMsg::HandOffOpen { .. }
            | AppMsg::OpenWithFiles(..)
            | AppMsg::DropFilesAs(..)
            | AppMsg::DropFiles(..)) => self.on_hand_off(msg, sender),
            msg @ (AppMsg::NewFromTemplate
            | AppMsg::TemplatePickNow
            | AppMsg::TemplatePicked(..)
            | AppMsg::SaveDraftMessage(..)
            | AppMsg::SaveTemplateMessage(..)
            | AppMsg::AutoSaveDraft { .. }
            | AppMsg::DiscardAutosave { .. }
            | AppMsg::DeleteDraft { .. }
            | AppMsg::DraftSaved { .. }) => self.on_drafts(msg, sender),
            msg @ (AppMsg::ShowSettingsPage(..)
            | AppMsg::OpenSettings
            | AppMsg::SettingsPageShown(..)
            | AppMsg::OpenPreferences
            | AppMsg::ClosePreferences
            | AppMsg::DebugCloseSettings
            | AppMsg::PrewarmSettings
            | AppMsg::SettingsEditorOpen(..)
            | AppMsg::SettingsLeaveEditor { .. }
            | AppMsg::ServerIdentities { .. }
            | AppMsg::WantIdentities(..)) => self.on_settings_window(msg, sender),
            msg @ (AppMsg::OpenMessageFile(..)
            | AppMsg::PickMessageFile
            | AppMsg::OpenMid(..)
            | AppMsg::MidLocated { .. }) => self.on_uri_queue(msg, sender),
            msg @ (AppMsg::FindTags
            | AppMsg::WantQuota(..)
            | AppMsg::QuotaFound { .. }
            | AppMsg::KeywordsFound { .. }
            | AppMsg::TagScanTimeout(..)
            | AppMsg::SetTags(..)
            | AppMsg::TagSelected { .. }
            | AppMsg::KeywordsSynced { .. }
            | AppMsg::TagViewLoaded { .. }) => self.on_tags(msg, sender),
            msg @ (AppMsg::SendMessage { .. }
            | AppMsg::DispatchSend(..)
            | AppMsg::UndoSend(..)
            | AppMsg::SendDone { .. }
            | AppMsg::Sent { .. }
            | AppMsg::Unsent { .. }) => self.on_send(msg, sender),
            msg @ (AppMsg::FillUnifiedSlices
            | AppMsg::Preload
            | AppMsg::PrebuildViews
            | AppMsg::PrebuildNext
            | AppMsg::UnifiedFoldersLoaded { .. }
            | AppMsg::PushUnreadCounts
            | AppMsg::EmitUnifiedNow
            | AppMsg::EmitUnified) => self.on_unified(msg, sender),
            msg @ (AppMsg::SendMorphMeasured | AppMsg::SendMorphFailed) => self.on_send_morph(msg),
            msg @ (AppMsg::ComposeClosed(..)
            | AppMsg::ComposeCloseRequested(..)
            | AppMsg::ComposeToggleWindow(..)) => self.on_compose_host(msg, sender),
            msg @ (AppMsg::OpenAccounts
            | AppMsg::AddFirstAccount
            | AppMsg::AccountSaved { .. }
            | AppMsg::ShowKeyringHelp { .. }
            | AppMsg::ReconnectAccounts
            | AppMsg::ShowCarryOverNotice(..)
            | AppMsg::AccountEnabledChanged { .. }
            | AppMsg::ApplyWelcomePrefs(..)
            | AppMsg::ImportGoaAccount(..)
            | AppMsg::AccountRemoved { .. }
            | AppMsg::GoaChanged(..)
            | AppMsg::SetAccount(..)) => self.on_accounts(msg, sender),
            msg @ (AppMsg::OpenConsole | AppMsg::ExportLogTo(..)) => self.on_diagnostics(msg),
            msg @ (AppMsg::ApplyFilters(..)
            | AppMsg::FilterRunTimeout
            | AppMsg::FilterRunToBackground
            | AppMsg::FolderSynced { .. }) => self.on_filter_run(msg, sender),
            msg @ (AppMsg::SmartSelected(..)
            | AppMsg::SmartViewLoaded { .. }
            | AppMsg::FlagsSynced { .. }
            | AppMsg::CountSmartFolders
            | AppMsg::SmartCounts(..)
            | AppMsg::SetSmartFolders(..)) => self.on_smart(msg, sender),
            msg @ (AppMsg::RefreshUnifiedStarred | AppMsg::UnifiedStarredLoaded { .. }) => {
                self.on_unified_starred(msg, sender)
            }
            msg @ (AppMsg::SetTag { .. }
            | AppMsg::SetTagMany { .. }
            | AppMsg::MoveToInbox
            | AppMsg::ToggleTagCurrent(..)
            | AppMsg::TagKey { .. }
            | AppMsg::MoveToMenu
            | AppMsg::ListMoveTo { .. }
            | AppMsg::MoveMessagesTo { .. }
            | AppMsg::MoveSelectionTo { .. }) => self.on_move_tag(msg, sender),
            msg @ (AppMsg::ReaderTagMenu | AppMsg::CardMenu { .. }) => {
                self.on_card_menu(msg, sender)
            }
            msg @ AppMsg::ImportSettingsFrom(..) => self.on_prefs_data(msg),
            msg @ (AppMsg::SetFolders { .. }
            | AppMsg::FolderUnread { .. }
            | AppMsg::SeenSettled { .. }
            | AppMsg::Answered { .. }
            | AppMsg::TagSettled { .. }
            | AppMsg::MovesSettled { .. }
            | AppMsg::FolderUnreadByPath { .. }
            | AppMsg::BodyHits { .. }) => self.on_events_folders(msg, sender),
            msg @ (AppMsg::RawExported { .. } | AppMsg::RawImported { .. }) => {
                self.on_transfer(msg, sender)
            }
            msg @ (AppMsg::Messages { .. }
            | AppMsg::MessagesAppend { .. }
            | AppMsg::BackfillDone { .. }
            | AppMsg::MessageGone { .. }
            | AppMsg::RefsRepaired { .. }) => self.on_events_messages(msg, sender),
            msg @ (AppMsg::SenderChecked { .. }
            | AppMsg::Body { .. }
            | AppMsg::Source { .. }
            | AppMsg::Attachments { .. }
            | AppMsg::AttachmentsPending { .. }
            | AppMsg::NoAttachments { .. }
            | AppMsg::HasAttachments { .. }) => self.on_events_bodies(msg, sender),
            AppMsg::Pref(pref) => self.route_pref(pref, sender),
        }
    }

    /// Hand a settings change to the module that applies it.
    fn route_pref(&mut self, pref: PrefOutput, sender: ComponentSender<Self>) {
        match pref {
            pref @ (PrefOutput::SetFolderSort(..)
            | PrefOutput::SetSidebarHoverExpand(..)
            | PrefOutput::SetRememberSidebar(..)
            | PrefOutput::SetRememberRail(..)
            | PrefOutput::SetSeenCounts(..)
            | PrefOutput::SetRailDots(..)
            | PrefOutput::SetRailFold(..)
            | PrefOutput::SetShowUnified(..)
            | PrefOutput::SetUnifiedChips(..)
            | PrefOutput::SetUnifiedKinds(..)
            | PrefOutput::SetShowAccounts(..)
            | PrefOutput::SetSectionStyle(..)
            | PrefOutput::SetFilteredPlacement(..)
            | PrefOutput::SetTagsPlacement(..)
            | PrefOutput::SetFavoritesPlacement(..)
            | PrefOutput::SetContactsPlacement(..)
            | PrefOutput::SetStartView(..)
            | PrefOutput::SetChevronsLeft(..)
            | PrefOutput::SetAccountClickFolds(..)) => self.on_prefs_sidebar_pref(pref),
            pref @ (PrefOutput::SetAppTheme(..)
            | PrefOutput::SetTextScale(..)
            | PrefOutput::SetTheme(..)
            | PrefOutput::SetAvatars(..)
            | PrefOutput::SetFocusMode(..)
            | PrefOutput::SetTagRowTint(..)
            | PrefOutput::SetOwnMailboxFace(..)
            | PrefOutput::SetSenderLogos(..)
            | PrefOutput::SetDateStyle(..)
            | PrefOutput::SetRenderer(..)
            | PrefOutput::SetLanguage(..)
            | PrefOutput::SetClockStyle(..)
            | PrefOutput::SetGravatar(..)
            | PrefOutput::SetFetchInterval(..)
            | PrefOutput::SetMeteredSaveData(..)
            | PrefOutput::SetPush(..)
            | PrefOutput::SetNotifications(..)
            | PrefOutput::SetNotificationContent(..)
            | PrefOutput::SetNotifyReceipts(..)
            | PrefOutput::SetNotificationButtons(..)
            | PrefOutput::SetRunInBackground(..)
            | PrefOutput::SetAutostart(..)
            | PrefOutput::SetTray(..)
            | PrefOutput::SetTrayIcon(..)
            | PrefOutput::SetAppIcon(..)
            | PrefOutput::SetTrayMail(..)
            | PrefOutput::SetLauncherBadge(..)
            | PrefOutput::SetSingleKey(..)
            | PrefOutput::SetStartFullscreen(..)
            | PrefOutput::SetKeepDownloads(..)
            | PrefOutput::SetGmailKeys(..)
            | PrefOutput::SetDesktopIcons(..)
            | PrefOutput::SetReadMark(..)
            | PrefOutput::SetStatusBarTop(..)
            | PrefOutput::SetConsoleMode(..)
            | PrefOutput::SetSettingsOpenAccounts(..)) => self.on_prefs_general_pref(pref, sender),
            pref @ (PrefOutput::SetContactBook(..)
            | PrefOutput::LoadContactBooks
            | PrefOutput::ImportLocalContacts
            | PrefOutput::ExportLocalContacts
            | PrefOutput::DeleteAllLocalContacts
            | PrefOutput::ExportSettings
            | PrefOutput::ExportLog
            | PrefOutput::ImportSettings
            | PrefOutput::ContactSourcesChanged) => self.on_prefs_data_pref(pref, sender),
            pref @ (PrefOutput::SetReaderToolbar(..)
            | PrefOutput::SetToolbarFoldRight(..)
            | PrefOutput::SetShowRemoteBanner(..)
            | PrefOutput::SetShowSpoofBanner(..)
            | PrefOutput::SetReceipts(..)
            | PrefOutput::SetAutoRemoteContent(..)
            | PrefOutput::SetRemoteAhead(..)
            | PrefOutput::SetThreading(..)
            | PrefOutput::SetThreadExpansion(..)
            | PrefOutput::SetThreadRowNewest(..)
            | PrefOutput::SetConfirmThreadDelete(..)
            | PrefOutput::SetCardActionsMode { .. }
            | PrefOutput::SetThreadsExpanded(..)
            | PrefOutput::SetThreadNewestFirst(..)
            | PrefOutput::SetFoldMessages(..)
            | PrefOutput::SetAlwaysShowRecipients(..)
            | PrefOutput::SetCardAttachments(..)
            | PrefOutput::SetAttachmentDrawer(..)
            | PrefOutput::SetReaderDefault(..)
            | PrefOutput::SetPgpLabels(..)
            | PrefOutput::SetAccountChip(..)
            | PrefOutput::SetReaderFindBottom(..)
            | PrefOutput::SetSingleMessageCard(..)
            | PrefOutput::SetCardPaletteCollapse(..)
            | PrefOutput::SetCardPaletteMenu(..)
            | PrefOutput::SetMessageTheme(..)
            | PrefOutput::SetOverrideFonts(..)
            | PrefOutput::SetReaderFont(..)
            | PrefOutput::SetPlainMonospace(..)
            | PrefOutput::SetPlainFont(..)
            | PrefOutput::SetOverrideColors(..)) => self.on_prefs_reader_pref(pref),
            pref @ (PrefOutput::SetListLayout(..)
            | PrefOutput::SetListHeadings(..)
            | PrefOutput::SetListColumnScroll(..)
            | PrefOutput::SetListSearchOpen(..)
            | PrefOutput::SetContactsSearchOpen(..)
            | PrefOutput::SetContactNames(..)
            | PrefOutput::SetListSearchBottom(..)
            | PrefOutput::SetListBulkBarBottom(..)
            | PrefOutput::SetGroupByDate(..)
            | PrefOutput::SetPillsBelow(..)
            | PrefOutput::SetListColumns(..)
            | PrefOutput::SetPreviewLines(..)
            | PrefOutput::SetAttachmentsRow(..)
            | PrefOutput::ResetColumnWidths
            | PrefOutput::SetPaletteCollapse(..)
            | PrefOutput::SetListPalette(..)
            | PrefOutput::SetListPaletteHover(..)
            | PrefOutput::SetSwipeEnabled(..)
            | PrefOutput::SetSwipeReversed(..)
            | PrefOutput::SetSwipeSensitivity(..)) => self.on_prefs_list_pref(pref, sender),
            pref @ (PrefOutput::SetComposeInline(..)
            | PrefOutput::SetReplyInline(..)
            | PrefOutput::SetReplyFields(..)
            | PrefOutput::SetFilesPrefs(..)
            | PrefOutput::SetLinkBrowser(..)
            | PrefOutput::SetComposeDefaultFrom(..)
            | PrefOutput::SetPastePlain(..)
            | PrefOutput::SetReturnParagraph(..)
            | PrefOutput::SetToolbarExpanded(..)
            | PrefOutput::SetComposeFont(..)
            | PrefOutput::SetComposeFontSize(..)
            | PrefOutput::SetComposeZoom(..)
            | PrefOutput::SetPrintOptions(..)
            | PrefOutput::SetSpellcheck(..)
            | PrefOutput::SetSpellcheckLangs(..)
            | PrefOutput::SetComposeFormat(..)
            | PrefOutput::SetFollowPlain(..)
            | PrefOutput::SetReplyPosition(..)
            | PrefOutput::SetSignaturePosition(..)
            | PrefOutput::SetSignatureDashes(..)
            | PrefOutput::SetReplyHeader(..)
            | PrefOutput::SetUndoSend(..)
            | PrefOutput::SetRequestReceipts(..)
            | PrefOutput::SetReturnedReceipts(..)) => self.on_prefs_compose_pref(pref),
            // The rest of Preferences' outputs are mapped to their own
            // messages before they get here.
            _ => {}
        }
    }
}
