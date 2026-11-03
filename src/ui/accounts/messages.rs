//! The Accounts panel's messages: what the app and the settings window
//! send it, what it reports back, and the results of its background work.

use super::*;

#[derive(Debug)]
pub(crate) enum AccountsInput {
    /// Read GNOME Online Accounts again, for accounts added there since the
    /// list was made: the refresh button, and GOA saying one was added.
    RefreshGoa,
    /// An account's folder order was changed from the sidebar: this copy of
    /// the account, and its editor if open, follow, so saving the editor
    /// later doesn't put the old order back.
    SetFolderSort {
        email: String,
        sort: Option<crate::config::FolderSort>,
    },
    /// The editor's "Use my Gravatar" switch moved (#189).
    SetOwnGravatar(bool),
    /// The "Server saves its own copy of sent mail" switch: with it on there is
    /// no copy of Hylki's to file, so the folder row above has nothing to say.
    SetServerSavesSent(bool),
    /// Showcase only (HYLKI_SHOWCASE_EDITOR_DIRTY): type into the open
    /// editor's Label field, the way a capture cannot.
    DebugEditLabel(String),
    /// Showcase only (HYLKI_SHOWCASE_ACCOUNT=new:<label>): pick the provider
    /// with that label in the open editor.
    DebugProvider(String),
    /// The settings sidebar wants to show another category while an editor
    /// is open. The answer (below) says whether anything would be lost.
    LeaveRequest(String),
    /// What the open editor answered: leave quietly, or ask the user first.
    LeaveVerdict {
        page: String,
        touched: bool,
    },
    /// The settings sidebar chose one of this component's pages (#141):
    /// "accounts", "tags", "filters" or "senders".
    ShowPage(String),
    /// The filters page's search text changed.
    SearchFilters(String),
    /// The senders page's search text changed (both lists).
    SearchSenders(String),
    /// Leave the account editor without saving (the settings window moved on).
    CloseEditor,
    /// Drop the signature editor's WebKit view when no account editor is
    /// showing: it is rebuilt on the next visit, and its web process (a few
    /// hundred MB, outside the sandbox's view) otherwise lives as long as
    /// the hidden Settings window does, which is until the app exits (#221).
    ReleaseSignatureEditor,
    /// The app's live folder lists per account email (for Special Folders).
    SetFolderChoices(std::collections::HashMap<String, Vec<(String, String)>>),
    AddAccount,
    /// Add the On This Computer account (#552).
    AddLocal,
    /// Pick a Maildir tree, or with `true` mbox files, to import into it.
    ImportLocal(bool),
    EditAccount(usize),
    /// Open the editor for the account with this address (the sidebar's
    /// "Account Settings…"), leaving another account's editor if one is up.
    EditAccountByEmail(String),
    /// The email field changed — mirror it into the (auto-filled) label field.
    EmailChanged,
    MoveRow {
        from: usize,
        to: usize,
    },
    /// Enable/disable an account from the list toggle.
    ToggleEnabled {
        index: usize,
        enabled: bool,
    },
    /// Enable/disable the account currently open in the editor (GOA group toggle).
    ToggleCurrentEnabled(bool),
    /// Import a GNOME Online Account (by index into `goa`) into Hylki.
    ImportGoa(usize),
    /// The provider dropdown changed — adapt the form (servers vs. OAuth).
    ProviderChanged,
    /// The Incoming Protocol row changed: JMAP has no SMTP side.
    ProtocolChanged,
    /// Start the OAuth browser sign-in flow.
    OAuthSignIn,
    /// The "Search the organization directory" switch moved (#410).
    SetGraphDirectory(bool),
    /// The "Show sensitivity label names" switch moved (#411).
    SetGraphLabels(bool),
    /// Open GNOME Settings → Online Accounts (the Google path).
    OpenOnlineAccounts,
    SetEmoji(String),
    /// Choose an avatar picture from disk (#162).
    PickAvatar,
    /// A picture was imported under this file name.
    SetAvatar(String),
    /// Clear what the circle shows in the current mode: the emoji, or
    /// the picture.
    ClearGlyph,
    /// The "Circle shows" toggle: picture (true) or initials/emoji.
    SetPictureMode(bool),
    /// Redraw the preview circle from the editor's current values.
    RefreshPreview,
    TestConnection,
    Save,
    /// Second phase of Save, once the signature HTML has been read from the editor.
    SaveWithSig(String),
    /// "Edit HTML" in the signature editor's toolbar: show the signature
    /// as its HTML source, or ("Preview") as rich text again.
    SignatureSourceToggle,
    /// The signature as HTML, read out of the rich editor: show it as source.
    SignatureToSource(String),
    /// The signature's source, read out of the editor: show it as rich text.
    SignatureToRich(String),
    /// "Import File…" under the signature: pick an HTML or text file.
    SignatureImport,
    /// A signature file was chosen.
    SignatureImportFile(std::path::PathBuf),
    /// The signature chooser moved (#445): show the one it is on.
    SigShow,
    /// "Add Signature": a new, empty one, shown for editing.
    SigAdd,
    /// Delete the signature shown.
    SigDelete,
    /// The name field changed.
    SigRename,
    /// The "Start messages with this signature" switch changed.
    SigDefault,
    /// The WebView's dirty flag, read as a signature is left.
    SigTextTouched(bool),
    /// The shown signature's HTML, read back as it is left; `add` puts a
    /// new one up next, otherwise the chooser's says which.
    SigStored {
        html: String,
        add: bool,
    },
    /// Clicked "Remove Account" — ask for confirmation first.
    RemoveCurrent,
    /// Confirmed in the dialog — actually remove the account being edited.
    ConfirmRemove,
    /// Open the alias dialog to add a new send-as alias (#34).
    AliasAdd,
    /// Open the alias dialog on an existing alias (by index into `alias_edits`).
    AliasEdit(usize),
    /// Remove an alias from the list being edited.
    AliasRemove(usize),
    /// Bring a hidden folder back (#239): the row's index in the list.
    UnhideFolder(usize),
    /// The alias dialog's Save button.
    AliasDialogSave,
    /// The alias dialog's Test button: try its SMTP server and credentials.
    AliasDialogTest,
    /// The alias dialog was closed (Cancel, Esc, or after a save).
    AliasDialogClosed,
    /// Allow list / blocklist / filters (moved here from Settings).
    AddSenderText(String),
    RemoveSenderRow(String),
    AddBlacklistText(String),
    RemoveBlacklistRow(String),
    AddFilter,
    /// Open the filter page on a new rule started from this draft, as the
    /// "Create Filter from Message…" entry does.
    AddFilterFrom(crate::config::FilterRule),
    /// Run the rules over the mail already in the inboxes (#198).
    ApplyFilters,
    /// The app says whether that run is still going (#198).
    FiltersApplying(bool),
    RemoveFilter(usize),
    /// Save whichever editor page is up (account, filter or tag): the
    /// settings window's leave-editor prompt.
    SaveOpenPage,
    /// Open the filter page on an existing rule.
    EditFilter(usize),
    /// The dialog saved changes to the rule at this index.
    FilterEdited(usize, crate::config::FilterRule),
    /// Toggle whether a rule's folder is listed under All Inboxes.
    FilterAdded(crate::config::FilterRule),
    /// Tags (#71): the dialog, and what it hands back.
    AddTag,
    EditTag(usize),
    RemoveTag(usize),
    /// A tag row was dragged onto another: reorder (#157). The order is the
    /// sidebar's, and the 1–9 shortcuts'.
    MoveTag {
        from: usize,
        to: usize,
    },
    TagAdded(crate::config::Tag),
    TagEdited(usize, crate::config::Tag),
    /// Smart folders (#417): the editor, what it hands back, and the list
    /// as the app has it (one deleted from the sidebar).
    AddSmartFolder,
    EditSmartFolder(usize),
    RemoveSmartFolder(usize),
    SmartFolderSaved(Option<usize>, crate::config::SmartFolder),
    SetSmartFolders(Vec<crate::config::SmartFolder>),
    /// The tag finder: "Find Tags…" was pressed (the app scans every
    /// account and answers with `TagFindings`).
    FindTags,
    /// The scan is under way (or over): dim the button meanwhile.
    TagScanning(bool),
    /// What the scan found that is not a tag here yet, ready to offer.
    TagFindings(Vec<TagProposal>),
    /// The tags the user chose from the report.
    ImportTags(Vec<crate::config::Tag>),
    /// How much storage an account's server says is in use (#298); shown
    /// when that account's page is the one open.
    Quota {
        email: String,
        quota: Option<crate::models::MailboxQuota>,
    },
    /// The sending identities an account's JMAP server keeps (#346), as
    /// (name, address): listed under its aliases, read-only.
    ServerIdentities {
        email: String,
        identities: Vec<(String, String)>,
    },
}

/// One keyword the tag finder proposes as a tag: the tag as it would be
/// added (name, keyword, color) and where the keyword was seen.
#[derive(Debug, Clone)]
pub(crate) struct TagProposal {
    pub(crate) tag: crate::config::Tag,
    /// Messages carrying it, where known (0 = not counted).
    pub(crate) count: usize,
    /// "Folder" or "account: Folder" entries, for the report's subtitle.
    pub(crate) places: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum AccountsOutput {
    /// `original_email` is `Some` (the pre-edit email) when editing, `None` when adding.
    Saved {
        original_email: Option<String>,
        account: Box<AccountConfig>,
    },
    Removed {
        email: String,
    },
    /// Import mail into On This Computer (#552): Maildir trees, or with
    /// `mbox`, mbox files.
    ImportMail {
        sources: Vec<std::path::PathBuf>,
        mbox: bool,
    },
    /// New display order, as account emails.
    Reordered(Vec<String>),
    /// An account was enabled/disabled from the list.
    EnabledChanged {
        email: String,
        enabled: bool,
    },
    /// Import a GNOME Online Account into Hylki (with its credentials).
    ImportGoa(Box<AccountConfig>),
    /// An editor subpage opened (the settings page it belongs to: accounts,
    /// filters or tags) or closed (`None`) — the combined settings window
    /// hides its shared header while one is open.
    EditorOpen(Option<&'static str>),
    /// Mail-hygiene changes, routed to the same app handlers Settings used.
    AddSender(String),
    RemoveSender(String),
    AddBlacklist(String),
    RemoveBlacklist(String),
    SetFilters(Vec<crate::config::FilterRule>),
    /// Apply the rules to mail that is already there (#198).
    ApplyFilters,
    SetTags(Vec<crate::config::Tag>),
    SetSmartFolders(Vec<crate::config::SmartFolder>),
    /// The tag finder wants every account scanned for keywords in use.
    FindTags,
    /// An account's page opened: ask its server for the storage in use.
    WantQuota(String),
    /// An account's page opened: the identities its server keeps (#346).
    WantIdentities(String),
    /// The editor was left with nothing changed in it: it is already closed,
    /// and the settings window can show `page` without asking anything.
    LeftEditor(String),
    /// The editor holds unsaved changes: the settings window should ask
    /// before showing `page`.
    LeaveNeedsPrompt(String),
}

/// How a sign-in ended: the refresh token and, when the provider says
/// (Microsoft), the name and address that signed in; or why it failed.
pub(super) type SignInResult = Result<(String, Option<(String, String)>), String>;

/// Background command results for the editor.
#[derive(Debug)]
pub(crate) enum AccountsCmd {
    /// Test-connection result.
    Test(ConnTest),
    /// A browser sign-in finished: the refresh token, and who signed in
    /// when the provider says (Microsoft); or why it failed.
    OAuth(SignInResult),
    /// The sign-in that asked for the directory's scopes (#410) finished,
    /// as [`AccountsCmd::OAuth`].
    DirectoryConsent(SignInResult),
    /// The one that asked for the label scope (#411).
    LabelsConsent(SignInResult),
    /// Alias SMTP test result (#34).
    AliasTested(Result<(), String>),
    /// GNOME Online Accounts read again: its mail accounts, for the import
    /// list.
    GoaListed(Vec<crate::services::goa::GoaMailAccount>),
    /// An account's secrets, read from the keyring for its editor.
    Secrets(u64, AccountSecrets),
    /// The Gravatar looked up for the address in the editor (#189).
    OwnGravatar {
        email: String,
        outcome: crate::services::avatar::FetchOutcome,
    },
}

/// Everything the Accounts panel needs at launch: the accounts themselves
/// plus the mail-hygiene lists that live on this tab (filters, allow list,
/// blocklist).
#[derive(Debug, Clone)]
pub(crate) struct AccountsInit {
    pub(crate) accounts: Vec<AccountConfig>,
    pub(crate) allowed_senders: Vec<String>,
    pub(crate) blacklist: Vec<String>,
    pub(crate) filters: Vec<crate::config::FilterRule>,
    pub(crate) tags: Vec<crate::config::Tag>,
    pub(crate) smart_folders: Vec<crate::config::SmartFolder>,
}
