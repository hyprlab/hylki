//! Accounts panel: manage all mail accounts (add / edit / remove / reorder).
//!
//! Not a window of its own: the panel is embedded behind the "Accounts" tab
//! of the combined Accounts & Preferences window (see `ui/preferences.rs`).
//! It uses an `AdwNavigationView` with two pages: a list of accounts (drag rows
//! to set the sidebar order) and a reusable editor form pushed on top.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{split_identity, AccountConfig, AliasConfig, OAuthSettings, Protocol};
use crate::ui::rich_editor::{self, RichEditor};
use crate::ui::preferences::{SenderRow, SenderRowOutput};
use crate::worker::{self, ConnTest};
use crate::i18n::{i18n, i18n_f, i18n_noop};

const DEFAULT_COLOR: &str = "#3584e4";

/// How an account signs in, chosen via the single Provider dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderKind {
    /// Manual IMAP/POP3 + password ("IMAP/POP3 Account").
    Manual,
    /// A known IMAP provider: password auth with auto-filled servers.
    Preset,
    /// Google OAuth (browser sign-in; falls back to GNOME Online Accounts).
    Google,
    /// Microsoft OAuth (browser sign-in).
    Microsoft,
    /// OAuth against a user-entered provider ("Custom (OAuth)…").
    CustomOAuth,
}

/// One entry in the Provider dropdown. It selects both the sign-in method and,
/// for `Preset`, the IMAP/SMTP servers to auto-fill. `hint` is shown as the row's
/// subtitle. Server fields are empty for non-`Preset` kinds (OAuth providers get
/// their servers from `crate::oauth::preset`; Manual/Custom are user-entered).
pub(crate) struct Provider {
    label: &'static str,
    /// The brand id of its mark (`brand::image_or`): "mail", the blue
    /// envelope, for manual IMAP/POP3; "mail-oauth", the yellow one, for
    /// custom OAuth.
    brand: &'static str,
    kind: ProviderKind,
    imap_host: &'static str,
    imap_port: u16,
    smtp_host: &'static str,
    smtp_port: u16,
    hint: &'static str,
}

impl Provider {
    /// Wizard accessors (src/ui/welcome.rs): the fields stay private to this
    /// module, which owns the table's meaning.
    pub(crate) fn wizard_password_provider(&self) -> bool {
        self.is_password()
    }
    pub(crate) fn wizard_label(&self) -> &'static str {
        self.label
    }
    /// The plain IMAP/POP3 entry, the wizard's default.
    pub(crate) fn wizard_is_manual(&self) -> bool {
        self.kind == ProviderKind::Manual
    }
    pub(crate) fn wizard_servers(&self) -> (&'static str, u16, &'static str, u16) {
        (self.imap_host, self.imap_port, self.smtp_host, self.smtp_port)
    }
    pub(crate) fn wizard_hint(&self) -> &'static str {
        self.hint
    }

    fn is_password(&self) -> bool {
        matches!(self.kind, ProviderKind::Manual | ProviderKind::Preset)
    }
    fn is_oauth(&self) -> bool {
        !self.is_password()
    }
    /// OAuth preset key for the built-in providers.
    fn oauth_name(&self) -> Option<&'static str> {
        match self.kind {
            ProviderKind::Google => Some("google"),
            ProviderKind::Microsoft => Some("microsoft"),
            _ => None,
        }
    }
}

const APP_PW: &str = i18n_noop("Requires an app-specific password (not your normal login password).");

/// The Provider dropdown, in display order. The plain IMAP/POP3 entry first
/// (the default), then the OAuth options, the major app-password IMAP
/// providers, and custom OAuth last. IMAP uses SSL/TLS on 993; SMTP uses
/// implicit TLS on 465 or STARTTLS on 587.
pub(crate) const PROVIDERS: &[Provider] = &[
    Provider { label: "IMAP/POP3 Account", brand: "mail", kind: ProviderKind::Manual, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Enter your server details manually.") },
    Provider { label: "Google (Gmail) — sign in", brand: "gmail", kind: ProviderKind::Google, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Sign in with your browser — no password needed.") },
    Provider { label: "Microsoft 365 / Outlook", brand: "outlook", kind: ProviderKind::Microsoft, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Sign in through GNOME Online Accounts.") },
    Provider { label: "iCloud", brand: "icloud", kind: ProviderKind::Preset, imap_host: "imap.mail.me.com", imap_port: 993, smtp_host: "smtp.mail.me.com", smtp_port: 587, hint: APP_PW },
    Provider { label: "Yahoo Mail", brand: "yahoo", kind: ProviderKind::Preset, imap_host: "imap.mail.yahoo.com", imap_port: 993, smtp_host: "smtp.mail.yahoo.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "Proton Mail (Bridge)", brand: "proton", kind: ProviderKind::Preset, imap_host: "127.0.0.1", imap_port: 1143, smtp_host: "127.0.0.1", smtp_port: 1025, hint: i18n_noop("Requires Proton Mail Bridge running locally.") },
    Provider { label: "Fastmail", brand: "fastmail", kind: ProviderKind::Preset, imap_host: "imap.fastmail.com", imap_port: 993, smtp_host: "smtp.fastmail.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "AOL Mail", brand: "aol", kind: ProviderKind::Preset, imap_host: "imap.aol.com", imap_port: 993, smtp_host: "smtp.aol.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "Zoho Mail", brand: "zoho", kind: ProviderKind::Preset, imap_host: "imap.zoho.com", imap_port: 993, smtp_host: "smtp.zoho.com", smtp_port: 465, hint: "" },
    Provider { label: "GMX", brand: "gmx", kind: ProviderKind::Preset, imap_host: "imap.gmx.com", imap_port: 993, smtp_host: "mail.gmx.com", smtp_port: 587, hint: i18n_noop("Enable POP/IMAP access in GMX settings first.") },
    Provider { label: "Yandex Mail", brand: "yandex", kind: ProviderKind::Preset, imap_host: "imap.yandex.com", imap_port: 993, smtp_host: "smtp.yandex.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "Mail.com", brand: "mailcom", kind: ProviderKind::Preset, imap_host: "imap.mail.com", imap_port: 993, smtp_host: "smtp.mail.com", smtp_port: 587, hint: "" },
    Provider { label: "Custom (OAuth)…", brand: "mail-oauth", kind: ProviderKind::CustomOAuth, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Enter your provider's OAuth endpoints, then sign in.") },
];

/// Dropdown index of the "IMAP/POP3 Account" manual entry (the default).
fn manual_index() -> u32 {
    PROVIDERS
        .iter()
        .position(|p| p.kind == ProviderKind::Manual)
        .unwrap_or(0) as u32
}

/// The provider entry for a dropdown index (clamped to the manual default).
fn provider_at(idx: u32) -> &'static Provider {
    PROVIDERS
        .get(idx as usize)
        .unwrap_or(&PROVIDERS[manual_index() as usize])
}

pub struct AccountsWindow {
    /// Accounts in display order.
    accounts: Vec<AccountConfig>,
    /// Each account's live folder list (path, display name), for the editor's
    /// Special Folders combos — pushed by the app, keyed by account email.
    folders_by_email: std::collections::HashMap<String, Vec<(String, String)>>,
    /// Allowed-senders and blocklist rows (moved here from Settings).
    senders: relm4::factory::FactoryVecDeque<SenderRow>,
    sender_addrs: Vec<String>,
    blacklist: relm4::factory::FactoryVecDeque<SenderRow>,
    blacklist_addrs: Vec<String>,
    /// Filter rules (#47), managed on this tab.
    filter_rules: Vec<crate::config::FilterRule>,
    filters_list: Option<gtk::ListBox>,
    /// The filters page's search text, read by the list's filter function.
    filters_query: std::rc::Rc<std::cell::RefCell<String>>,
    /// The open filter or tag page's Save: reads the form and hands the
    /// result back, true when it went through (the page then pops). The
    /// settings window's leave-editor prompt saves through it too.
    form_save: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn() -> bool>>>>,
    /// The senders page's search text, shared by both lists.
    senders_query: std::rc::Rc<std::cell::RefCell<String>>,
    /// Tags (#71), managed on this tab too — the rules' "Tag with" names them.
    tags: Vec<crate::config::Tag>,
    tags_list: Option<gtk::ListBox>,
    /// The tag finder's button: its spinner turns while the mailboxes are
    /// being read, and its label says so.
    find_tags_spinner: Option<gtk::Spinner>,
    /// The Apply Now button's spinner and label, turned on while a manual
    /// filter run is under way (#198).
    apply_filters_spinner: Option<gtk::Spinner>,
    apply_filters_label: Option<gtk::Label>,
    /// Whether a manual filter run is under way; a second press does nothing.
    filters_applying: bool,
    find_tags_label: Option<gtk::Label>,
    tag_scanning: bool,
    /// Paths behind the currently-open editor's folder combos (index 0 in the
    /// combo is "Automatic"; entry N here is combo index N + 1).
    folder_paths: Vec<String>,
    /// Paths behind the "Save a copy of sent mail in" combo (#199). A separate list from
    /// `folder_paths`: this one offers every folder, the Inbox included,
    /// because a destination takes nothing away from the folder it names.
    /// Index 0 in the combo is "Sent folder"; entry N here is index N + 1.
    sent_copy_paths: Vec<String>,
    /// Index being edited; `None` while adding a new account.
    editing: Option<usize>,
    /// Emoji currently chosen in the editor (`None` → use initials).
    emoji: Option<String>,
    /// Avatar picture chosen in the editor (#162): its file name under the
    /// avatars directory.
    avatar: Option<String>,
    /// The preview circle's colour, as a stylesheet on the display.
    preview_css: gtk::CssProvider,
    /// The account list's per-row disc colours (`.acct-list-color-N`),
    /// rewritten on every rebuild.
    list_css: gtk::CssProvider,
    /// The "Circle shows" toggle: a picture, or initials/emoji. Only the
    /// chosen side is saved; the other side's choice stays in the editor
    /// so flipping back and forth loses nothing.
    picture_mode: bool,
    /// What the account editor held when it opened, so leaving an untouched
    /// one asks nothing. `None` while no editor is open — or when something
    /// filled the form without recording it, where the safe answer is that
    /// it was touched.
    editor_seed: Option<String>,
    /// WYSIWYG editor for the account signature.
    /// The signature's rich editor — a WebKit view, so it is created when
    /// an account's editor first opens rather than with the panel.
    sig_editor: Option<RichEditor>,
    /// The email value the label field currently mirrors, so the label auto-fills
    /// from the email until the user customizes it.
    label_synced: String,
    /// GNOME Online Accounts mail accounts available to import (not yet in Hylki).
    goa: Vec<crate::goa::GoaMailAccount>,
    /// Refresh token captured from a successful OAuth sign-in, applied on save.
    pending_oauth_refresh: Option<String>,
    /// The send-as aliases being edited for the account in the editor (#34).
    /// Committed to the account on Save.
    alias_edits: Vec<AliasConfig>,
    /// Index into `alias_edits` open in the alias dialog; `None` while adding.
    alias_editing: Option<usize>,
    /// The open alias editor dialog and its fields, if any.
    alias_dialog: Option<AliasDialog>,
}

/// The alias editor dialog (#34): a small modal for one send-as alias — its
/// identity, and optionally its own SMTP transport.
struct AliasDialog {
    window: adw::Window,
    name_row: adw::EntryRow,
    addr_row: adw::EntryRow,
    smtp_switch: adw::SwitchRow,
    host_row: adw::EntryRow,
    port_row: adw::EntryRow,
    user_row: adw::EntryRow,
    pass_row: adw::PasswordEntryRow,
    test_btn: gtk::Button,
    test_result: gtk::Label,
}

#[derive(Debug)]
pub enum AccountsInput {
    /// The editor's "Use my Gravatar" switch moved (#189).
    SetOwnGravatar(bool),
    /// The "Server saves its own copy of sent mail" switch: with it on there is
    /// no copy of Hylki's to file, so the folder row above has nothing to say.
    SetServerSavesSent(bool),
    /// Showcase only (HYLKI_SHOWCASE_EDITOR_DIRTY): type into the open
    /// editor's Label field, the way a capture cannot.
    DebugEditLabel(String),
    /// The settings sidebar wants to show another category while an editor
    /// is open. The answer (below) says whether anything would be lost.
    LeaveRequest(String),
    /// What the open editor answered: leave quietly, or ask the user first.
    LeaveVerdict { page: String, touched: bool },
    /// The settings sidebar chose one of this component's pages (#141):
    /// "accounts", "tags", "filters" or "senders".
    ShowPage(String),
    /// The filters page's search text changed.
    SearchFilters(String),
    /// The senders page's search text changed (both lists).
    SearchSenders(String),
    /// Leave the account editor without saving (the settings window moved on).
    CloseEditor,
    /// The app's live folder lists per account email (for Special Folders).
    SetFolderChoices(std::collections::HashMap<String, Vec<(String, String)>>),
    AddAccount,
    EditAccount(usize),
    /// Open the editor for the account with this address (the sidebar's
    /// "Account Settings…"), leaving another account's editor if one is up.
    EditAccountByEmail(String),
    /// The email field changed — mirror it into the (auto-filled) label field.
    EmailChanged,
    MoveRow { from: usize, to: usize },
    /// Enable/disable an account from the list toggle.
    ToggleEnabled { index: usize, enabled: bool },
    /// Enable/disable the account currently open in the editor (GOA group toggle).
    ToggleCurrentEnabled(bool),
    /// Import a GNOME Online Account (by index into `goa`) into Hylki.
    ImportGoa(usize),
    /// The provider dropdown changed — adapt the form (servers vs. OAuth).
    ProviderChanged,
    /// Start the OAuth browser sign-in flow.
    OAuthSignIn,
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
    /// "Edit HTML…" under the signature: read the editor, then open the source dialog.
    SignatureEditSource,
    /// The editor's current HTML, for the source dialog.
    SignatureSourceLoaded(String),
    /// The source dialog's Apply: sanitize the text and load it into the editor.
    SignatureApplySource(String),
    /// "Import File…" under the signature: pick an HTML or text file.
    SignatureImport,
    /// A signature file was chosen.
    SignatureImportFile(std::path::PathBuf),
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
    MoveTag { from: usize, to: usize },
    TagAdded(crate::config::Tag),
    TagEdited(usize, crate::config::Tag),
    /// The tag finder: "Find Tags…" was pressed (the app scans every
    /// account and answers with `TagFindings`).
    FindTags,
    /// The scan is under way (or over): dim the button meanwhile.
    TagScanning(bool),
    /// What the scan found that is not a tag here yet, ready to offer.
    TagFindings(Vec<TagProposal>),
    /// The tags the user chose from the report.
    ImportTags(Vec<crate::config::Tag>),
}

/// One keyword the tag finder proposes as a tag: the tag as it would be
/// added (name, keyword, colour) and where the keyword was seen.
#[derive(Debug, Clone)]
pub struct TagProposal {
    pub tag: crate::config::Tag,
    /// Messages carrying it, where known (0 = not counted).
    pub count: usize,
    /// "Folder" or "account: Folder" entries, for the report's subtitle.
    pub places: Vec<String>,
}

#[derive(Debug)]
pub enum AccountsOutput {
    /// `original_email` is `Some` (the pre-edit email) when editing, `None` when adding.
    Saved {
        original_email: Option<String>,
        account: Box<AccountConfig>,
    },
    Removed { email: String },
    /// New display order, as account emails.
    Reordered(Vec<String>),
    /// An account was enabled/disabled from the list.
    EnabledChanged { email: String, enabled: bool },
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
    /// The tag finder wants every account scanned for keywords in use.
    FindTags,
    /// The editor was left with nothing changed in it: it is already closed,
    /// and the settings window can show `page` without asking anything.
    LeftEditor(String),
    /// The editor holds unsaved changes: the settings window should ask
    /// before showing `page`.
    LeaveNeedsPrompt(String),
}

/// Whether a GOA account's mail runs over the Microsoft Graph API: the
/// "Microsoft 365" (`ms_graph`) provider has no IMAP — its token is
/// Graph-scoped — so the imported account uses [`Protocol::Graph`] (issue #36).
/// Stop a row's subtitle from squeezing out the row's value.
///
/// An [`adw::ActionRow`] gives its title block whatever width the text asks
/// for, and an [`adw::ComboRow`]'s value label takes what is left — which for
/// a subtitle of any length is an ellipsis where "Disabled" should be. Capping
/// the label's width in characters bounds what it can ask for, so it wraps to
/// a second line instead of taking the room from its neighbour. `subtitle-lines`
/// does not do this: it caps how many lines are drawn, not how wide one is.
fn wrap_subtitle(row: &impl IsA<gtk::Widget>, chars: i32) {
    fn walk(w: &gtk::Widget, chars: i32) -> bool {
        if let Some(label) = w.downcast_ref::<gtk::Label>() {
            if label.has_css_class("subtitle") {
                label.set_wrap(true);
                label.set_max_width_chars(chars);
                return true;
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if walk(&c, chars) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    walk(row.as_ref(), chars);
}

/// The subtitle under "Save a copy of sent mail in" (#199). Says the one thing
/// the row above it cannot: this only files mail somewhere, so the Inbox is a
/// fair answer here even though giving it the Sent *role* would cost the
/// account its inbox. What "Disabled" leaves the copy to is left to the title,
/// which already says "a copy": the originals are the Sent row's business.
/// Declared once because the row is rebuilt whenever an editor opens, and the
/// two copies drifting apart is how a hint goes stale.
const SENT_COPY_HINT: &str =
    "Any folder, inbox included. Only applies to mail sent from this Hylki client \
     going forward. Not recursive.";

fn goa_uses_graph(g: &crate::goa::GoaMailAccount) -> bool {
    g.oauth2 && g.provider_type == "ms_graph"
}

/// GNOME Online Accounts mail accounts not (properly) configured in Hylki.
/// A configured entry counts when it has an IMAP host or runs over Graph — an
/// entry with neither is a broken pre-#36 Microsoft 365 import, so its GOA
/// account is offered again and re-importing repairs it. Accounts GOA can
/// neither serve IMAP nor Graph mail for can never connect and aren't listed.
fn importable_goa_accounts(configured: &[AccountConfig]) -> Vec<crate::goa::GoaMailAccount> {
    crate::goa::list_mail_accounts()
        .into_iter()
        .filter(|g| {
            !configured.iter().any(|a| {
                a.email.eq_ignore_ascii_case(&g.email)
                    && (!a.imap_host.trim().is_empty()
                        || a.protocol == crate::config::Protocol::Graph)
            })
        })
        .filter(|g| !g.imap_host.is_empty() || goa_uses_graph(g))
        .collect()
}

/// Background command results for the editor.
#[derive(Debug)]
pub enum AccountsCmd {
    /// Test-connection result.
    Test(ConnTest),
    /// OAuth sign-in result: the refresh token, or an error message.
    OAuth(Result<String, String>),
    /// Alias SMTP test result (#34).
    AliasTested(Result<(), String>),
    /// An account's secrets, read from the keyring for its editor.
    Secrets(AccountSecrets),
    /// The Gravatar looked up for the address in the editor (#189).
    OwnGravatar {
        email: String,
        outcome: crate::avatar::FetchOutcome,
    },
}

/// What the keyring holds for one account: its password, its own SMTP
/// password when SMTP is separate, and each alias's SMTP password (by the
/// alias address, #34). Read off the main thread when the editor opens, so
/// the Settings window never waits on the keyring.
#[derive(Debug, Clone)]
pub struct AccountSecrets {
    pub email: String,
    pub password: String,
    pub smtp_password: String,
    pub aliases: Vec<(String, String)>,
}

impl AccountSecrets {
    /// The keyring entries `acc` lacks (blocking; run off the main thread).
    fn load(acc: &AccountConfig) -> AccountSecrets {
        let password = if acc.oauth || !acc.password.is_empty() {
            acc.password.clone()
        } else {
            crate::config::load_password(&acc.email).unwrap_or_default()
        };
        let smtp_password = if acc.smtp_separate && acc.smtp_password.is_empty() {
            crate::config::load_smtp_password(&acc.email).unwrap_or_default()
        } else {
            acc.smtp_password.clone()
        };
        let aliases = acc
            .aliases
            .iter()
            .filter(|al| al.has_own_smtp() && al.smtp_password.is_empty())
            .map(|al| {
                let addr = al.address();
                let pw = crate::config::load_alias_smtp_password(&acc.email, &addr)
                    .unwrap_or_default();
                (addr, pw)
            })
            .collect();
        AccountSecrets { email: acc.email.clone(), password, smtp_password, aliases }
    }

    /// Whether `acc` still needs anything from the keyring.
    fn needed(acc: &AccountConfig) -> bool {
        (!acc.oauth && acc.password.is_empty())
            || (acc.smtp_separate && acc.smtp_password.is_empty())
            || acc.aliases.iter().any(|al| al.has_own_smtp() && al.smtp_password.is_empty())
    }

    /// Write the secrets into `acc` where it has none.
    fn apply(&self, acc: &mut AccountConfig) {
        if acc.password.is_empty() {
            acc.password = self.password.clone();
        }
        if acc.smtp_password.is_empty() {
            acc.smtp_password = self.smtp_password.clone();
        }
        for al in acc.aliases.iter_mut() {
            if al.smtp_password.is_empty() {
                if let Some((_, pw)) = self.aliases.iter().find(|(a, _)| *a == al.address()) {
                    al.smtp_password = pw.clone();
                }
            }
        }
    }
}

/// Whether a list row survives the page's search text (#141): matched
/// against the row's title and, for an action row, its subtitle.
fn row_matches(row: &gtk::ListBoxRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let title = row
        .downcast_ref::<adw::PreferencesRow>()
        .map(|r| r.title().to_lowercase())
        .unwrap_or_default();
    let subtitle = row
        .downcast_ref::<adw::ActionRow>()
        .and_then(|r| r.subtitle())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    title.contains(query) || subtitle.contains(query)
}

/// Everything the Accounts panel needs at launch: the accounts themselves
/// plus the mail-hygiene lists that live on this tab (filters, allow list,
/// blocklist).
#[derive(Debug, Clone)]
pub struct AccountsInit {
    pub accounts: Vec<AccountConfig>,
    pub allowed_senders: Vec<String>,
    pub blacklist: Vec<String>,
    pub filters: Vec<crate::config::FilterRule>,
    pub tags: Vec<crate::config::Tag>,
}

#[relm4::component(pub)]
impl Component for AccountsWindow {
    type Init = AccountsInit;
    type Input = AccountsInput;
    type Output = AccountsOutput;
    type CommandOutput = AccountsCmd;

    view! {
        adw::Bin {
            #[wrap(Some)]
            #[name = "nav"]
            set_child = &adw::NavigationView {

                // ---- list page ----
                add = &adw::NavigationPage {
                    set_title: &i18n("Accounts"),
                    set_tag: Some("list"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        // No header of its own: the combined settings window's
                        // shared header (with the view switcher) sits above.

                        #[wrap(Some)]
                        #[name = "list_stack"]
                        set_content = &gtk::Stack {
                            // Sections switch outright, and only the shown
                            // one is measured.
                            set_transition_type: gtk::StackTransitionType::None,
                            set_hhomogeneous: false,
                            set_vhomogeneous: false,

                            add_named[Some("accounts")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Mail Accounts"),
                                    set_description: Some(
                                        i18n("Drag to set the order they appear in the sidebar.").as_str()
                                    ),
                                    // At the header's end, as the Tags and
                                    // Cloud Storage panels place theirs.
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_valign: gtk::Align::Start,
                                        set_halign: gtk::Align::End,
                                        set_margin_start: 24,
                                        gtk::Button {
                                            set_label: &i18n("Add Account…"),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::AddAccount,
                                        },
                                    },

                                    #[name = "accounts_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                        connect_row_activated[sender] => move |_, row| {
                                            sender.input(AccountsInput::EditAccount(row.index() as usize));
                                        },
                                    },
                                },

                                #[name = "goa_group"]
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("GNOME Online Accounts"),
                                    set_description: Some(
                                        i18n("Mail accounts from GNOME Settings. Toggle one on to \
                                         use it in Hylki.").as_str()
                                    ),
                                    set_visible: false,

                                    #[name = "goa_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            add_named[Some("tags")] = &adw::PreferencesPage {
                                // Tags (#71): a name and colour per keyword.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Tags"),
                                    set_description: Some(
                                        i18n("Label messages with one or more coloured tags. \
                                         Tags are stored on the mail server as IMAP keywords, \
                                         so Thunderbird and other clients show the same tags.\n\n\
                                         Drag a tag to reorder: the order here is the sidebar's, \
                                         and the first nine answer to the 1–9 keys.").as_str()
                                    ),
                                    #[wrap(Some)]
                                    // Stacked like the Cloud Storage panel's
                                    // buttons: a column at the header's end.
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,
                                        set_valign: gtk::Align::Start,
                                        set_halign: gtk::Align::End,
                                        set_margin_start: 24,
                                        // Both 130px wide (filling the column,
                                        // so they always share one width), and
                                        // the finder's label change moves nothing.
                                        gtk::Button {
                                            set_label: &i18n("Add Tag…"),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::AddTag,
                                        },
                                        // The tag finder: read every mailbox
                                        // for keywords already in use and
                                        // offer them as tags.
                                        #[name = "find_tags_btn"]
                                        gtk::Button {
                                            set_tooltip_text: Some(i18n("Look through every mailbox for tags other clients have set").as_str()),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::FindTags,
                                            // A spinner inside the button
                                            // while the mailboxes are read.
                                            gtk::Box {
                                                set_spacing: 6,
                                                set_halign: gtk::Align::Center,
                                                #[name = "find_tags_spinner"]
                                                gtk::Spinner {
                                                    set_visible: false,
                                                },
                                                #[name = "find_tags_label"]
                                                gtk::Label {
                                                    set_label: &i18n("Find Tags…"),
                                                },
                                            },
                                        },
                                    },

                                    #[name = "tags_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            add_named[Some("filters")] = &adw::PreferencesPage {
                                // A search box over the long lists (#141): rows
                                // that do not match the text are hidden.
                                add = &adw::PreferencesGroup {
                                    #[name = "filters_search"]
                                    gtk::SearchEntry {
                                        set_placeholder_text: Some(i18n("Search filters").as_str()),
                                        connect_search_changed[sender] => move |entry| {
                                            sender.input(AccountsInput::SearchFilters(entry.text().to_string()));
                                        },
                                    },
                                },

                                // Mail hygiene (moved from Settings): filters,
                                // the remote-content allow list, the blocklist.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Filters"),
                                    set_description: Some(
                                        i18n("File incoming mail into folders or tag it automatically, \
                                         by sender, subject or recipients. Applied to each \
                                         account's Inbox as Hylki syncs it, or to the mail already \
                                         there with Apply Now; a folder's right-click menu runs \
                                         them over that one folder.").as_str()
                                    ),
                                    // At the header's end, like the other
                                    // panels' buttons.
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 6,
                                        set_valign: gtk::Align::Start,
                                        set_halign: gtk::Align::End,
                                        set_margin_start: 24,
                                        gtk::Button {
                                            set_label: &i18n("Add Filter…"),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::AddFilter,
                                        },
                                        // Rules normally meet mail as it
                                        // arrives; this holds them up against
                                        // what is already in the Inbox (#198).
                                        #[name = "apply_filters_btn"]
                                        gtk::Button {
                                            set_size_request: (130, -1),
                                            set_tooltip_text: Some(
                                                i18n("Run every rule over the mail already in your inboxes").as_str()
                                            ),
                                            connect_clicked => AccountsInput::ApplyFilters,
                                            // A spinner inside the button
                                            // while the run is under way, as
                                            // the tag finder has.
                                            gtk::Box {
                                                set_spacing: 6,
                                                set_halign: gtk::Align::Center,
                                                #[name = "apply_filters_spinner"]
                                                gtk::Spinner {
                                                    set_visible: false,
                                                },
                                                #[name = "apply_filters_label"]
                                                gtk::Label {
                                                    set_label: &i18n("Apply Now"),
                                                },
                                            },
                                        },
                                    },

                                    #[name = "filters_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            add_named[Some("senders")] = &adw::PreferencesPage {
                                // A search box over the long lists (#141): rows
                                // that do not match the text are hidden.
                                add = &adw::PreferencesGroup {
                                    #[name = "senders_search"]
                                    gtk::SearchEntry {
                                        set_placeholder_text: Some(i18n("Search senders").as_str()),
                                        connect_search_changed[sender] => move |entry| {
                                            sender.input(AccountsInput::SearchSenders(entry.text().to_string()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Allowed Senders"),
                                    set_description: Some(
                                        i18n("Messages from these senders load remote content \
                                         automatically.").as_str()
                                    ),

                                    #[name = "add_sender_row"]
                                    adw::EntryRow {
                                        set_title: &i18n("Email address"),
                                        set_input_purpose: gtk::InputPurpose::Email,
                                        set_show_apply_button: false,
                                        connect_entry_activated[sender] => move |row| {
                                            sender.input(AccountsInput::AddSenderText(row.text().to_string()));
                                            row.set_text("");
                                        },

                                        add_suffix = &gtk::Button {
                                            set_icon_name: "co.hyprlab.Hylki-list-add-symbolic",
                                            set_tooltip_text: Some(i18n("Allow this sender").as_str()),
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "flat",
                                            connect_clicked[sender, add_sender_row] => move |_| {
                                                sender.input(AccountsInput::AddSenderText(
                                                    add_sender_row.text().to_string(),
                                                ));
                                                add_sender_row.set_text("");
                                            },
                                        },
                                    },

                                    #[local_ref]
                                    senders_box -> gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        add_css_class: "sender-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Blacklist"),
                                    set_description: Some(
                                        i18n("Incoming mail from these senders is deleted \
                                         automatically (moved to Trash). Enter an email \
                                         address, or a whole domain like \"example.com\" \
                                         to block every sender there.").as_str()
                                    ),

                                    #[name = "add_blacklist_row"]
                                    adw::EntryRow {
                                        set_title: &i18n("Address or domain"),
                                        set_show_apply_button: false,
                                        connect_entry_activated[sender] => move |row| {
                                            sender.input(AccountsInput::AddBlacklistText(row.text().to_string()));
                                            row.set_text("");
                                        },

                                        add_suffix = &gtk::Button {
                                            set_icon_name: "co.hyprlab.Hylki-list-add-symbolic",
                                            set_tooltip_text: Some(i18n("Block this sender").as_str()),
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "flat",
                                            connect_clicked[sender, add_blacklist_row] => move |_| {
                                                sender.input(AccountsInput::AddBlacklistText(
                                                    add_blacklist_row.text().to_string(),
                                                ));
                                                add_blacklist_row.set_text("");
                                            },
                                        },
                                    },

                                    #[local_ref]
                                    blacklist_box -> gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        add_css_class: "sender-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },
                        },
                    },
                },

                // ---- editor page ---- (taken out of the view until an
                // editor opens: see `mount_editor`)
                #[name = "editor_page"]
                add = &adw::NavigationPage {
                    set_title: &i18n("Account"),
                    set_tag: Some("editor"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            // The window's close button lives here while the
                            // editor is up (the shared header hides), so it
                            // is never out of reach.
                            set_show_end_title_buttons: true,
                            pack_end = &gtk::Button {
                                set_label: &i18n("Save"),
                                add_css_class: "suggested-action",
                                connect_clicked => AccountsInput::Save,
                            },
                            // Left of Save, only while editing an existing
                            // account; asks before removing.
                            #[name = "remove_btn"]
                            pack_end = &gtk::Button {
                                set_label: &i18n("Remove"),
                                add_css_class: "destructive-action",
                                set_visible: false,
                                connect_clicked => AccountsInput::RemoveCurrent,
                            },
                        },

                        #[wrap(Some)]
                        set_content = &adw::PreferencesPage {
                            // GNOME Online Accounts owns this account's servers and
                            // credentials; Hylki only mirrors them. Saying so where
                            // the greyed-out fields are is worth more than leaving
                            // the user to work out why they can't type.
                            // GNOME Online Accounts owns this account's servers and
                            // credentials; Hylki only mirrors them, and can hide it
                            // locally. Both facts belong together, above the fields
                            // they explain.
                            // The provider's mark over the form, following
                            // the Provider picker (a generic envelope for
                            // manual IMAP and custom OAuth).
                            add = &adw::PreferencesGroup {
                                #[name = "provider_mark"]
                                gtk::Image {
                                    set_pixel_size: 56,
                                    set_halign: gtk::Align::Center,
                                    set_margin_bottom: 6,
                                },
                            },

                            #[name = "goa_banner"]
                            add = &adw::PreferencesGroup {
                                set_visible: false,
                                set_title: &i18n("GNOME Online Account"),

                                #[name = "goa_enabled_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Show in Hylki"),
                                    set_subtitle: &i18n("Switching this off returns the account to the \
                                                   import list — it stays in GNOME Online Accounts."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input(AccountsInput::ToggleCurrentEnabled(row.is_active()));
                                    },
                                },

                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 12,
                                    set_halign: gtk::Align::Start,
                                    set_margin_top: 12,

                                    gtk::Label {
                                        set_label: &i18n("This account is managed by GNOME Online Accounts.\nIts address, servers and password are changed in Settings \u{2192} Online Accounts."),
                                        set_xalign: 0.0,
                                        set_halign: gtk::Align::Start,
                                        set_wrap: true,
                                        add_css_class: "dim-label",
                                    },

                                    gtk::Button {
                                        set_label: &i18n("Open Online Accounts\u{2026}"),
                                        set_halign: gtk::Align::Start,
                                        connect_clicked => AccountsInput::OpenOnlineAccounts,
                                    },
                                },
                            },

                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Mail Account"),

                                // Pick the provider first; the rest of the form
                                // adapts (server fields vs. OAuth sign-in).
                                #[name = "provider_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Provider"),
                                    set_subtitle: &i18n("Choose your email provider."),
                                    connect_selected_notify => AccountsInput::ProviderChanged,
                                },
                                #[name = "name_row"]
                                adw::EntryRow { set_title: &i18n("Display Name") },
                                #[name = "email_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Email Address"),
                                    set_input_purpose: gtk::InputPurpose::Email,
                                },
                                #[name = "protocol_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Incoming Protocol"),
                                },
                                #[name = "host_row"]
                                adw::EntryRow { set_title: &i18n("Incoming Server") },
                                #[name = "port_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Port (IMAP 993 / POP3 995)"),
                                    set_input_purpose: gtk::InputPurpose::Digits,
                                },
                                #[name = "smtp_row"]
                                adw::EntryRow { set_title: &i18n("SMTP Server (optional)") },
                                #[name = "smtp_port_row"]
                                adw::EntryRow {
                                    set_title: &i18n("SMTP Port (default 587)"),
                                    set_input_purpose: gtk::InputPurpose::Digits,
                                },
                                #[name = "user_row"]
                                adw::EntryRow { set_title: &i18n("Username") },
                                #[name = "pass_row"]
                                adw::PasswordEntryRow { set_title: &i18n("Password") },

                                // ---- OAuth fields (shown when Authentication is an OAuth option) ----
                                #[name = "oauth_client_id_row"]
                                adw::EntryRow {
                                    set_title: &i18n("OAuth Client ID"),
                                    set_visible: false,
                                },
                                #[name = "oauth_secret_row"]
                                adw::PasswordEntryRow {
                                    set_title: &i18n("OAuth Client Secret (optional)"),
                                    set_visible: false,
                                },
                                #[name = "oauth_auth_url_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Authorization URL"),
                                    set_visible: false,
                                },
                                #[name = "oauth_token_url_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Token URL"),
                                    set_visible: false,
                                },
                                #[name = "oauth_scope_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Scopes (space-separated)"),
                                    set_visible: false,
                                },
                                #[name = "oauth_signin_btn"]
                                gtk::Button {
                                    set_label: &i18n("Sign In with Browser"),
                                    set_halign: gtk::Align::Start,
                                    set_margin_top: 16,
                                    set_visible: false,
                                    add_css_class: "suggested-action",
                                    connect_clicked => AccountsInput::OAuthSignIn,
                                },
                                #[name = "oauth_status"]
                                gtk::Label {
                                    set_visible: false,
                                    set_halign: gtk::Align::Start,
                                    set_xalign: 0.0,
                                    set_wrap: true,
                                },

                                // Shown for Google/Microsoft when no built-in/own OAuth
                                // client is available: point the user at GNOME Online
                                // Accounts (the only sign-in path for these providers).
                                #[name = "goa_hint"]
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 12,
                                    set_margin_top: 8,
                                    set_visible: false,

                                    gtk::Label {
                                        set_wrap: true,
                                        set_xalign: 0.0,
                                        add_css_class: "dim-label",
                                        set_label: &i18n("Google and Microsoft sign-in use GNOME Online \
                                            Accounts.\n\n\
                                            1. Open Online Accounts and sign in there.\n\
                                            2. Come back to Hylki and reopen this window — the \
                                            account then appears under “GNOME Online \
                                            Accounts” at the top of this window. Enable it there."),
                                    },
                                    gtk::Button {
                                        set_label: &i18n("Open Online Accounts…"),
                                        set_halign: gtk::Align::Start,
                                        add_css_class: "suggested-action",
                                        connect_clicked => AccountsInput::OpenOnlineAccounts,
                                    },
                                },

                                #[name = "smtp_separate_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Separate SMTP credentials"),
                                    set_subtitle: &i18n("Use a different username and password for \
                                                   sending. Off = use the credentials above."),
                                },
                                #[name = "smtp_user_row"]
                                adw::EntryRow { set_title: &i18n("SMTP Username") },
                                #[name = "smtp_pass_row"]
                                adw::PasswordEntryRow { set_title: &i18n("SMTP Password") },

                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 6,
                                    set_margin_top: 16,

                                    #[name = "test_btn"]
                                    gtk::Button {
                                        set_label: &i18n("Test Connection"),
                                        set_halign: gtk::Align::Start,
                                        connect_clicked => AccountsInput::TestConnection,
                                    },
                                    #[name = "test_result"]
                                    gtk::Label {
                                        set_visible: false,
                                        set_halign: gtk::Align::Start,
                                        set_xalign: 0.0,
                                        set_wrap: true,
                                    },
                                },
                            },

                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Appearance"),
                                set_description: Some(
                                    i18n("How this account is shown in the sidebar and \
                                     the Inboxes view.").as_str()
                                ),

                                // The sidebar circle as it will look, as the
                                // group's first row (a non-row child would
                                // land below the rows): the accent colour,
                                // and the picture, emoji or initials it
                                // shows. Not a button — it neither activates
                                // nor selects.
                                adw::PreferencesRow {
                                    set_activatable: false,
                                    set_selectable: false,
                                    set_can_focus: false,
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 2,
                                        set_halign: gtk::Align::Center,
                                        set_margin_top: 10,
                                        set_margin_bottom: 10,
                                        set_margin_start: 10,
                                        set_margin_end: 10,

                                        #[name = "preview_disc"]
                                        gtk::Box {
                                            add_css_class: "account-circle",
                                            add_css_class: "account-preview-disc",
                                            set_size_request: (72, 72),
                                            set_halign: gtk::Align::Center,
                                            set_hexpand: false,
                                            set_overflow: gtk::Overflow::Hidden,
                                        },
                                    },
                                },

                                #[name = "label_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Label (defaults to email address)"),
                                },

                                // The accent stands on its own: it colours
                                // the circle, and marks the account's
                                // folders and mail elsewhere.
                                adw::ActionRow {
                                    set_title: &i18n("Account accent color"),
                                    set_subtitle: &i18n("The account circle background, and this account's \
                                                   marks in the sidebar and lists"),
                                    #[name = "color_btn"]
                                    add_suffix = &gtk::ColorDialogButton {
                                        set_valign: gtk::Align::Center,
                                        set_dialog: &gtk::ColorDialog::new(),
                                        connect_rgba_notify[sender] => move |_| {
                                            sender.input(AccountsInput::RefreshPreview);
                                        },
                                    },
                                },

                                // This mailbox's own Gravatar (#189), ahead
                                // of everything below — when the address has
                                // one. Off by default: the lookup tells a
                                // third party the address is in use here.
                                #[name = "gravatar_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Use my Gravatar"),
                                    set_subtitle: &i18n("Show the picture this address has at gravatar.com. \
                                                   Looking it up sends them a hash of the address, and \
                                                   if there is none, the circle falls back to the \
                                                   choice below."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input(AccountsInput::SetOwnGravatar(row.is_active()));
                                    },
                                },

                                // What the circle shows (#162): initials or
                                // an emoji, or a picture. The row below
                                // follows the choice.
                                adw::ActionRow {
                                    set_title: &i18n("Circle shows"),
                                    add_suffix = &gtk::Box {
                                        add_css_class: "linked",
                                        set_valign: gtk::Align::Center,
                                        #[name = "mode_glyph_btn"]
                                        gtk::ToggleButton {
                                            set_label: &i18n("Initials or emoji"),
                                            set_active: true,
                                        },
                                        #[name = "mode_picture_btn"]
                                        gtk::ToggleButton {
                                            set_label: &i18n("Picture"),
                                            set_group: Some(&mode_glyph_btn),
                                            connect_toggled[sender] => move |b| {
                                                sender.input(AccountsInput::SetPictureMode(b.is_active()));
                                            },
                                        },
                                    },
                                },

                                #[name = "emoji_row"]
                                adw::ActionRow {
                                    set_title: &i18n("Emoji"),
                                    set_subtitle: &i18n("Shown in the circle instead of the initials"),

                                    #[name = "emoji_btn"]
                                    add_suffix = &gtk::MenuButton {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Choose…"),
                                        #[wrap(Some)]
                                        set_popover = &gtk::EmojiChooser {
                                            connect_emoji_picked[sender] => move |_, text| {
                                                sender.input(AccountsInput::SetEmoji(text.to_string()));
                                            },
                                        },
                                    },
                                    #[name = "emoji_clear_btn"]
                                    add_suffix = &gtk::Button {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Use initials"),
                                        set_visible: false,
                                        connect_clicked => AccountsInput::ClearGlyph,
                                    },
                                },

                                #[name = "picture_row"]
                                adw::ActionRow {
                                    set_title: &i18n("Picture"),
                                    set_subtitle: &i18n("A photo or logo from this computer"),
                                    set_visible: false,
                                    add_suffix = &gtk::Button {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Choose…"),
                                        connect_clicked => AccountsInput::PickAvatar,
                                    },
                                    #[name = "avatar_clear_btn"]
                                    add_suffix = &gtk::Button {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Remove"),
                                        set_visible: false,
                                        connect_clicked => AccountsInput::ClearGlyph,
                                    },
                                },
                            },

                            // Send-as aliases (#34): extra From identities the
                            // composer offers, and replies to an alias answer
                            // from it. Each alias sends through this account's
                            // SMTP, or — for a forwarded mailbox whose provider
                            // would rewrite the sender — through its own.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Send-as aliases"),
                                set_description: Some(
                                    i18n("Extra addresses this account can send as. An alias \
                                     can use this account's SMTP server, or bring its own.").as_str()
                                ),

                                // At the header's end, styled like the Add
                                // Filter…, Add Tag… and cloud Add Account…
                                // buttons.
                                #[wrap(Some)]
                                set_header_suffix = &gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_valign: gtk::Align::Start,
                                    set_halign: gtk::Align::End,
                                    set_margin_start: 24,
                                    gtk::Button {
                                        set_label: &i18n("Add Alias…"),
                                        set_size_request: (130, -1),
                                        connect_clicked => AccountsInput::AliasAdd,
                                    },
                                },

                                #[name = "aliases_list"]
                                gtk::ListBox {
                                    add_css_class: "boxed-list",
                                    set_selection_mode: gtk::SelectionMode::None,
                                    connect_row_activated[sender] => move |_, row| {
                                        sender.input(AccountsInput::AliasEdit(row.index() as usize));
                                    },
                                },
                            },

                            // Per-account push override (#91): some servers
                            // mishandle IDLE, and one bad account shouldn't
                            // cost the good ones their instant delivery.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Syncing"),

                                #[name = "push_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Instant new mail (IMAP push)"),
                                    set_subtitle: &i18n("Turn off for servers that stall on push connections."),
                                },

                                // Auto-empty (#140): mail past the chosen age in
                                // Junk / Trash is deleted for good at each sync.
                                #[name = "empty_junk_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Empty Junk automatically"),
                                    set_subtitle: &i18n("Delete junk mail older than this for good, \
                                                   checked at each sync."),
                                },
                                #[name = "empty_trash_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Empty Trash automatically"),
                                    set_subtitle: &i18n("Delete mail in the Trash older than this for \
                                                   good, checked at each sync."),
                                },
                            },

                            // Manual special-folder mapping (#82): for servers
                            // whose Sent/Trash/… aren't detected, pin each role
                            // to one of the account's real folders. "Save
                            // copies in" (#199) rides with them because it is
                            // read as a refinement of Sent, but it is a
                            // different kind of thing: a role says what a
                            // folder *is*, and relabelling the Inbox costs the
                            // account its inbox (#136), which is why the Inbox
                            // is offered in that row and in none of the others.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Special Folders"),
                                set_description: Some(
                                    i18n("Which of this account's folders hold each role. Automatically \
                                          follows the server's own markings; pick a folder when a role \
                                          isn't detected or lands wrong. Nothing already on the server \
                                          moves: a role says where mail goes from now on.").as_str()
                                ),

                                #[name = "folder_sent_row"]
                                adw::ComboRow { set_title: &i18n("Sent") },
                                #[name = "folder_sent_copy_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Save a copy of sent mail in"),
                                    set_subtitle: &i18n(SENT_COPY_HINT),
                                },
                                // Gmail files a copy of anything sent through
                                // its SMTP, so Hylki's append makes a second
                                // one. Off by default: a server that does not
                                // do it, paired with a Hylki that has stopped
                                // appending, keeps no sent mail at all.
                                #[name = "server_saves_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Server saves its own copy of sent mail"),
                                    set_subtitle: &i18n("For Gmail and others that file sent mail \
                                                   themselves. Disables above Save a copy."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input(AccountsInput::SetServerSavesSent(row.is_active()));
                                    },
                                },
                                #[name = "folder_drafts_row"]
                                adw::ComboRow { set_title: &i18n("Drafts") },
                                #[name = "folder_trash_row"]
                                adw::ComboRow { set_title: &i18n("Trash") },
                                #[name = "folder_junk_row"]
                                adw::ComboRow { set_title: &i18n("Junk") },
                                #[name = "folder_archive_row"]
                                adw::ComboRow { set_title: &i18n("Archive") },
                            },

                            // OpenPGP (#133): which of the user's keys this
                            // account signs and decrypts with.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("OpenPGP"),
                                set_description: Some(
                                    i18n("The key that signs mail sent from this account and opens \
                                     what is encrypted to it. Keys are managed under OpenPGP in \
                                     the sidebar.").as_str()
                                ),
                                #[name = "pgp_key_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Key"),
                                    set_subtitle: &i18n("Automatic uses the key whose address matches."),
                                },
                            },

                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Signature"),
                                set_description: Some(
                                    i18n("Appended to new messages sent from this account.").as_str()
                                ),
                                // A designed signature usually exists as HTML
                                // already (#120): bring it in from its file, or
                                // paste and edit the source directly.
                                #[wrap(Some)]
                                set_header_suffix = &gtk::Box {
                                    set_spacing: 6,
                                    set_valign: gtk::Align::Center,
                                    gtk::Button {
                                        set_label: &i18n("Edit HTML…"),
                                        add_css_class: "flat",
                                        connect_clicked => AccountsInput::SignatureEditSource,
                                    },
                                    gtk::Button {
                                        set_label: &i18n("Import File…"),
                                        add_css_class: "flat",
                                        connect_clicked => AccountsInput::SignatureImport,
                                    },
                                },

                                #[name = "sig_holder"]
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_height_request: 180,
                                    set_margin_top: 6,
                                },
                            },

                            // For a GOA-imported account this removes it from
                            // Hylki only — it stays in GNOME Online Accounts and
                            // returns to the import list.
                            add = &adw::PreferencesGroup {
                                gtk::Label {
                                    set_wrap: true,
                                    set_xalign: 0.0,
                                    add_css_class: "dim-label",
                                    add_css_class: "caption",
                                    set_label: &i18n("Your password is stored in the system keyring \
                                                (secret-service), never in plain text on disk."),
                                },
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let t_init = std::time::Instant::now();
        let goa = importable_goa_accounts(&init.accounts);

        let senders = relm4::factory::FactoryVecDeque::builder()
            .launch(gtk::ListBox::new())
            .forward(sender.input_sender(), |out| match out {
                SenderRowOutput::Remove(addr) => AccountsInput::RemoveSenderRow(addr),
            });
        let blacklist = relm4::factory::FactoryVecDeque::builder()
            .launch(gtk::ListBox::new())
            .forward(sender.input_sender(), |out| match out {
                SenderRowOutput::Remove(addr) => AccountsInput::RemoveBlacklistRow(addr),
            });

        let mut model = AccountsWindow {
            accounts: init.accounts,
            editing: None,
            emoji: None,
            avatar: None,
            preview_css: gtk::CssProvider::new(),
            list_css: gtk::CssProvider::new(),
            picture_mode: false,
            editor_seed: None,
            sig_editor: None,
            label_synced: String::new(),
            goa,
            pending_oauth_refresh: None,
            alias_edits: Vec::new(),
            alias_editing: None,
            alias_dialog: None,
            folders_by_email: std::collections::HashMap::new(),
            folder_paths: Vec::new(),
            sent_copy_paths: Vec::new(),
            senders,
            sender_addrs: Vec::new(),
            blacklist,
            blacklist_addrs: Vec::new(),
            filter_rules: init.filters,
            filters_list: None,
            filters_query: Default::default(),
            form_save: Default::default(),
            senders_query: Default::default(),
            tags: init.tags,
            tags_list: None,
            find_tags_spinner: None,
            apply_filters_spinner: None,
            apply_filters_label: None,
            filters_applying: false,
            find_tags_label: None,
            tag_scanning: false,
        };
        {
            let mut guard = model.senders.guard();
            for addr in &init.allowed_senders {
                model.sender_addrs.push(addr.clone());
                guard.push_back(addr.clone());
            }
        }
        {
            let mut guard = model.blacklist.guard();
            for addr in &init.blacklist {
                model.blacklist_addrs.push(addr.clone());
                guard.push_back(addr.clone());
            }
        }

        let senders_box = model.senders.widget();
        let blacklist_box = model.blacklist.widget();
        // An empty boxed list still draws its frame: a stray line under the
        // entry that adds to it (#129). Shown only once it has a row.
        senders_box.set_visible(!model.sender_addrs.is_empty());
        blacklist_box.set_visible(!model.blacklist_addrs.is_empty());
        let widgets = view_output!();
        // Long enough for the hint, short enough that the combo's own value
        // is not what gets shortened instead.
        wrap_subtitle(&widgets.folder_sent_copy_row, 34);
        // Left-justify the editor's wrapping labels (its group descriptions):
        // libadwaita 1.9 renders a group description fill-justified when its
        // text does not naturally fill the label's width, stretching the word
        // gaps (most visible on Special Folders, whose description spans the
        // full width). Single-line row labels are untouched.
        left_justify_wrapping_labels(&widgets.editor_page.clone().upcast());
        // The editor's sixty-odd rows stay out of the widget tree until an
        // editor opens, so the Settings window's first layout skips them.
        widgets.nav.remove(&widgets.editor_page);
        tracing::debug!("settings window: accounts view built in {:?}", t_init.elapsed());
        model.filters_list = Some(widgets.filters_list.clone());
        {
            let q = model.filters_query.clone();
            widgets.filters_list.set_filter_func(move |row| row_matches(row, &q.borrow()));
            let q = model.senders_query.clone();
            senders_box.set_filter_func(move |row| row_matches(row, &q.borrow()));
            let q = model.senders_query.clone();
            blacklist_box.set_filter_func(move |row| row_matches(row, &q.borrow()));
        }
        model.rebuild_filter_rows(&sender);
        model.tags_list = Some(widgets.tags_list.clone());
        model.find_tags_spinner = Some(widgets.find_tags_spinner.clone());
        model.apply_filters_spinner = Some(widgets.apply_filters_spinner.clone());
        model.apply_filters_label = Some(widgets.apply_filters_label.clone());
        model.find_tags_label = Some(widgets.find_tags_label.clone());
        model.rebuild_tag_rows(&sender);
        let t_list = std::time::Instant::now();
        model.rebuild_account_list(&widgets.accounts_list, &sender);
        tracing::debug!("settings window: accounts list built in {:?}", t_list.elapsed());
        model.rebuild_goa_list(&widgets.goa_list, &sender);
        widgets.goa_group.set_visible(!model.goa.is_empty());
        widgets
            .protocol_row
            .set_model(Some(&gtk::StringList::new(&["IMAP", "POP3"])));

        // The Provider dropdown picks both the sign-in method and (for known
        // providers) the servers. The default popup ellipsizes items; a factory
        // whose labels don't lets the list widen to the full option text.
        let provider_labels: Vec<&str> = PROVIDERS.iter().map(|p| p.label).collect();
        widgets
            .provider_row
            .set_model(Some(&gtk::StringList::new(&provider_labels)));
        widgets.provider_row.set_factory(Some(&provider_factory()));

        // Push override choices mirror AccountConfig::push (None / Some(true)
        // / Some(false), in that order).
        widgets
            .push_row
            .set_model(Some(&gtk::StringList::new(&[i18n("Follow Settings").as_str(), i18n("On").as_str(), i18n("Off").as_str()])));
        widgets.push_row.set_list_factory(Some(&non_ellipsizing_factory()));
        let empty_labels = auto_empty_labels();
        let empty_labels: Vec<&str> = empty_labels.iter().map(String::as_str).collect();
        for row in [&widgets.empty_junk_row, &widgets.empty_trash_row] {
            row.set_model(Some(&gtk::StringList::new(&empty_labels)));
            row.set_list_factory(Some(&non_ellipsizing_factory()));
        }

        // Show the SMTP credential fields only when the toggle is on.
        widgets
            .smtp_separate_row
            .bind_property("active", &widgets.smtp_user_row, "visible")
            .sync_create()
            .build();
        widgets
            .smtp_separate_row
            .bind_property("active", &widgets.smtp_pass_row, "visible")
            .sync_create()
            .build();

        // Auto-fill the label from the email as it's typed (until customized).
        let es = sender.clone();
        widgets.email_row.connect_changed(move |_| es.input(AccountsInput::EmailChanged));

        // The preview circle follows the name, label and email (its
        // initials) as they are typed; its colour is a stylesheet.
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &model.preview_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            gtk::style_context_add_provider_for_display(
                &display,
                &model.list_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        for row in [&widgets.name_row, &widgets.label_row, &widgets.email_row] {
            let s = sender.clone();
            row.connect_changed(move |_| s.input(AccountsInput::RefreshPreview));
        }

        // Tell the combined settings window when the editor subpage is up —
        // every way in or out (Save, back button, swipe) lands here.
        {
            let s = sender.output_sender().clone();
            let form_save = model.form_save.clone();
            widgets.nav.connect_visible_page_notify(move |nav| {
                let tag = nav.visible_page().and_then(|p| p.tag());
                let editor = match tag.as_deref() {
                    Some("editor") => Some("accounts"),
                    Some("filter") => Some("filters"),
                    Some("tag") => Some("tags"),
                    _ => None,
                };
                if !matches!(editor, Some("filters") | Some("tags")) {
                    form_save.borrow_mut().take();
                }
                let _ = s.send(AccountsOutput::EditorOpen(editor));
            });
        }

        tracing::debug!("settings window: accounts panel init {:?}", t_init.elapsed());
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            AccountsInput::ShowPage(id) => {
                widgets.list_stack.set_visible_child_name(&id);
            }
            AccountsInput::CloseEditor => {
                if widgets
                    .nav
                    .visible_page()
                    .and_then(|p| p.tag())
                    .is_some_and(|t| t == "editor" || t == "filter" || t == "tag")
                {
                    widgets.nav.pop();
                }
            }
            AccountsInput::DebugEditLabel(text) => widgets.label_row.set_text(&text),

            AccountsInput::LeaveRequest(page) => {
                match widgets.nav.visible_page().and_then(|p| p.tag()).as_deref() {
                    // The account editor knows what it was opened with, so it
                    // can tell an edited form from an untouched one. The
                    // signature lives in a WebView and only answers when
                    // asked, so that answer arrives separately.
                    Some("editor") => {
                        if self.editor_touched(widgets) {
                            sender.input(AccountsInput::LeaveVerdict { page, touched: true });
                        } else {
                            let s = sender.clone();
                            self.sig_editor(widgets).is_dirty(move |touched| {
                                s.input(AccountsInput::LeaveVerdict { page, touched });
                            });
                        }
                    }
                    // Nothing records what a filter or a tag was opened with,
                    // so those are always worth asking about.
                    Some("filter") | Some("tag") => {
                        sender.input(AccountsInput::LeaveVerdict { page, touched: true })
                    }
                    _ => sender.input(AccountsInput::LeaveVerdict { page, touched: false }),
                }
            }

            AccountsInput::LeaveVerdict { page, touched } => {
                if touched {
                    let _ = sender.output(AccountsOutput::LeaveNeedsPrompt(page));
                    return;
                }
                // Nothing to lose: close the editor and let the window move on.
                sender.input(AccountsInput::CloseEditor);
                let _ = sender.output(AccountsOutput::LeftEditor(page));
            }

            AccountsInput::SaveOpenPage => {
                match widgets.nav.visible_page().and_then(|p| p.tag()).as_deref() {
                    Some("editor") => sender.input(AccountsInput::Save),
                    Some("filter") | Some("tag") => {
                        let saved = self.form_save.borrow().as_ref().is_some_and(|f| f());
                        if saved {
                            widgets.nav.pop();
                        }
                    }
                    _ => {}
                }
            }
            AccountsInput::SearchFilters(text) => {
                *self.filters_query.borrow_mut() = text.to_lowercase();
                widgets.filters_list.invalidate_filter();
            }
            AccountsInput::SearchSenders(text) => {
                *self.senders_query.borrow_mut() = text.to_lowercase();
                self.senders.widget().invalidate_filter();
                self.blacklist.widget().invalidate_filter();
            }
            AccountsInput::AddAccount => {
                self.editing = None;
                self.emoji = None;
                self.avatar = None;
                widgets.gravatar_row.set_active(false);
                self.picture_mode = false;
                widgets.mode_glyph_btn.set_active(true);
                self.label_synced = String::new();
                self.pending_oauth_refresh = None;
                self.close_alias_dialog();
                self.alias_edits.clear();
                self.rebuild_alias_list(&widgets.aliases_list, &sender);
                clear_editor(widgets);
                self.populate_folder_combos(widgets, None);
                set_connection_editable(widgets, true);
                widgets.goa_banner.set_visible(false);
                self.apply_provider(widgets);
                self.sig_editor(widgets).set_html("");
                widgets.color_btn.set_rgba(&parse_color(DEFAULT_COLOR));
                self.refresh_preview(widgets);
                widgets.remove_btn.set_visible(false);
                // A prior GOA edit may have hidden the provider picker.
                widgets.provider_row.set_visible(true);
                self.editor_seed = Some(self.editor_fingerprint(widgets));
                mount_editor(widgets);
                widgets.nav.push_by_tag("editor");
            }

            AccountsInput::EditAccountByEmail(email) => {
                let Some(i) = self.accounts.iter().position(|a| a.email == email) else {
                    return;
                };
                let editor_up = widgets
                    .nav
                    .visible_page()
                    .and_then(|p| p.tag())
                    .is_some_and(|tag| tag == "editor");
                if editor_up {
                    if self.editing == Some(i) {
                        return;
                    }
                    // Another account's editor is up: back to the list first,
                    // since a page can't be pushed while it is in the stack.
                    widgets.nav.pop();
                }
                sender.input(AccountsInput::EditAccount(i));
            }

            AccountsInput::EditAccount(i) => {
                let Some(acc) = self.accounts.get(i).cloned() else {
                    return;
                };
                self.editing = Some(i);
                self.pending_oauth_refresh = None;
                self.close_alias_dialog();
                self.alias_edits = acc.aliases.clone();
                self.rebuild_alias_list(&widgets.aliases_list, &sender);
                fill_editor(widgets, &acc);
                // The secrets come from the keyring now, off the main thread,
                // and land in the fields when they arrive (Save reads the
                // keyring itself should it come first).
                if AccountSecrets::needed(&acc) {
                    let acc = acc.clone();
                    sender.oneshot_command(async move {
                        let email = acc.email.clone();
                        let secrets = tokio::task::spawn_blocking(move || AccountSecrets::load(&acc))
                            .await
                            .unwrap_or(AccountSecrets {
                                email,
                                password: String::new(),
                                smtp_password: String::new(),
                                aliases: Vec::new(),
                            });
                        AccountsCmd::Secrets(secrets)
                    });
                }
                self.populate_folder_combos(widgets, Some(&acc));
                self.apply_provider(widgets);
                // Label mirrors the email until customized.
                self.label_synced = acc.email.clone();
                self.sig_editor(widgets)
                    .set_html(&rich_editor::signature_to_html(acc.signature.as_deref().unwrap_or("")));
                widgets
                    .color_btn
                    .set_rgba(&parse_color(acc.color.as_deref().unwrap_or(DEFAULT_COLOR)));
                self.emoji = acc.emoji.clone();
                self.avatar = acc.avatar.clone();
                widgets.gravatar_row.set_active(acc.gravatar);
                self.picture_mode = acc.avatar.is_some();
                if self.picture_mode {
                    widgets.mode_picture_btn.set_active(true);
                } else {
                    widgets.mode_glyph_btn.set_active(true);
                }
                self.refresh_preview(widgets);
                // GOA accounts: no "Remove" (it lives in the system) — offer an
                // enable/disable toggle and a shortcut to Online Accounts instead.
                let is_goa = acc.goa_id.is_some();
                set_connection_editable(widgets, !is_goa);
                widgets.goa_banner.set_visible(is_goa);
                // GOA accounts get the same Remove flow — it removes the account
                // from Hylki only (back to the import list); GNOME keeps it.
                widgets.remove_btn.set_visible(true);
                // GNOME owns a GOA account's connection outright, so the server
                // and credential section isn't shown at all — only what Hylki
                // owns (name, label, colour, signature, aliases) plus the email
                // for identification. `apply_provider` re-shows what applies the
                // next time a native account or the add-form opens the editor.
                widgets.provider_row.set_visible(!is_goa);
                if is_goa {
                    for w in [
                        widgets.protocol_row.upcast_ref::<gtk::Widget>(),
                        widgets.host_row.upcast_ref(),
                        widgets.port_row.upcast_ref(),
                        widgets.smtp_row.upcast_ref(),
                        widgets.smtp_port_row.upcast_ref(),
                        widgets.user_row.upcast_ref(),
                        widgets.pass_row.upcast_ref(),
                        widgets.smtp_separate_row.upcast_ref(),
                        widgets.smtp_user_row.upcast_ref(),
                        widgets.smtp_pass_row.upcast_ref(),
                        widgets.test_btn.upcast_ref(),
                        widgets.oauth_signin_btn.upcast_ref(),
                        widgets.oauth_status.upcast_ref(),
                        widgets.goa_hint.upcast_ref(),
                    ] {
                        w.set_visible(false);
                    }
                    // The Google/Microsoft "use GNOME" guidance hid these; a GOA
                    // account edits its display fields right here.
                    widgets.name_row.set_visible(true);
                    widgets.email_row.set_visible(true);
                }
                widgets.goa_enabled_row.set_active(acc.enabled);
                // While Mail is switched off in GNOME Settings the account is
                // paused from there, not from here — say so where the toggle is.
                widgets.goa_enabled_row.set_sensitive(!acc.goa_mail_disabled);
                widgets.goa_enabled_row.set_subtitle(if acc.goa_mail_disabled {
                    "Paused: Mail is switched off for this account in GNOME Settings \u{2192} Online Accounts."
                } else {
                    "Switching this off returns the account to the import list — it \
                     stays in GNOME Online Accounts."
                });
                self.editor_seed = Some(self.editor_fingerprint(widgets));
                mount_editor(widgets);
                widgets.nav.push_by_tag("editor");
            }

            AccountsInput::EmailChanged => {
                let email = trimmed(&widgets.email_row);
                let label = widgets.label_row.text().to_string();
                // Mirror while the label is still tracking the email (or empty);
                // once the user types a custom label, stop.
                if label.is_empty() || label == self.label_synced {
                    widgets.label_row.set_text(&email);
                }
                self.label_synced = email;
            }

            AccountsInput::MoveRow { from, to } => {
                if from < self.accounts.len() {
                    let acc = self.accounts.remove(from);
                    let to = to.min(self.accounts.len());
                    self.accounts.insert(to, acc);
                    self.rebuild_account_list(&widgets.accounts_list, &sender);
                    let emails = self.accounts.iter().map(|a| a.email.clone()).collect();
                    let _ = sender.output(AccountsOutput::Reordered(emails));
                }
            }

            AccountsInput::ToggleEnabled { index, enabled } => {
                // A GNOME Online Account switched off here isn't paused — it is
                // un-imported: it drops out of Hylki (the config entry and its
                // stored copies go) and returns to the "GNOME Online Accounts"
                // list below, ready to import again. The account itself stays
                // in GNOME untouched.
                if !enabled && self.accounts.get(index).is_some_and(|a| a.goa_id.is_some()) {
                    self.unimport_goa(index, widgets, &sender);
                    return;
                }
                if let Some(acc) = self.accounts.get_mut(index) {
                    if acc.enabled != enabled {
                        acc.enabled = enabled;
                        let email = acc.email.clone();
                        let _ = sender.output(AccountsOutput::EnabledChanged { email, enabled });
                    }
                }
            }

            AccountsInput::ToggleCurrentEnabled(enabled) => {
                if let Some(i) = self.editing {
                    // Same un-import semantics as the list toggle; the editor
                    // page closes since its account is no longer in Hylki.
                    if !enabled && self.accounts.get(i).is_some_and(|a| a.goa_id.is_some()) {
                        self.unimport_goa(i, widgets, &sender);
                        widgets.nav.pop();
                        return;
                    }
                    if let Some(acc) = self.accounts.get_mut(i) {
                        if acc.enabled != enabled {
                            acc.enabled = enabled;
                            let email = acc.email.clone();
                            let _ = sender.output(AccountsOutput::EnabledChanged { email, enabled });
                            self.rebuild_account_list(&widgets.accounts_list, &sender);
                        }
                    }
                }
            }

            AccountsInput::ImportGoa(index) => {
                if let Some(g) = self.goa.get(index).cloned() {
                    // Password-based providers: pull the password now. OAuth
                    // providers: authenticate with a GOA token at connect time.
                    // Either way the worker asks GOA again when it connects, so an
                    // account still works if this read comes back empty.
                    let (password, oauth) = if g.password_based {
                        (crate::goa::mail_passwords(&g.id).0.unwrap_or_default(), false)
                    } else {
                        (String::new(), true)
                    };
                    let account = g.to_config(password, oauth);
                    self.goa.remove(index);
                    self.accounts.push(account.clone());
                    self.rebuild_account_list(&widgets.accounts_list, &sender);
                    self.rebuild_goa_list(&widgets.goa_list, &sender);
                    widgets.goa_group.set_visible(!self.goa.is_empty());
                    let _ = sender.output(AccountsOutput::ImportGoa(Box::new(account)));
                }
            }

            AccountsInput::SetEmoji(text) => {
                self.emoji = Some(text);
                self.refresh_preview(widgets);
            }

            AccountsInput::ClearGlyph => {
                if self.picture_mode {
                    self.avatar = None;
                } else {
                    self.emoji = None;
                }
                self.refresh_preview(widgets);
            }

            AccountsInput::SetServerSavesSent(on) => {
                widgets.folder_sent_copy_row.set_sensitive(!on);
            }

            AccountsInput::SetOwnGravatar(on) => {
                // Look it up as soon as it is asked for, so the preview can
                // answer rather than waiting for the account to be saved.
                let address = trimmed(&widgets.email_row);
                if on && !address.is_empty() && crate::avatar::wants_own_gravatar(&address) {
                    sender.oneshot_command(async move {
                        let outcome = tokio::task::spawn_blocking({
                            let address = address.clone();
                            move || crate::avatar::fetch_own_gravatar(&address)
                        })
                        .await
                        .unwrap_or(crate::avatar::FetchOutcome::Retry);
                        AccountsCmd::OwnGravatar { email: address, outcome }
                    });
                }
                self.refresh_preview(widgets);
            }

            AccountsInput::SetPictureMode(on) => {
                if self.picture_mode != on {
                    self.picture_mode = on;
                    self.refresh_preview(widgets);
                }
            }

            AccountsInput::RefreshPreview => self.refresh_preview(widgets),

            AccountsInput::PickAvatar => {
                let dialog = gtk::FileDialog::builder().title(&i18n("Choose a Picture")).build();
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(&i18n("Images")));
                filter.add_pixbuf_formats();
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));
                // The panel is a Bin inside the Settings window; the
                // dialog wants that window.
                let parent = root.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let s = sender.clone();
                dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    match import_avatar_file(&path) {
                        Ok(name) => s.input(AccountsInput::SetAvatar(name)),
                        Err(e) => tracing::warn!("could not import avatar {}: {e}", path.display()),
                    }
                });
            }

            AccountsInput::SetAvatar(name) => {
                self.avatar = Some(name);
                self.refresh_preview(widgets);
            }

            AccountsInput::TestConnection => {
                let account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
                widgets.test_btn.set_sensitive(false);
                widgets.test_result.set_visible(true);
                widgets.test_result.set_css_classes(&["dim-label"]);
                widgets.test_result.set_label(&i18n("Testing…"));
                sender.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        worker::test_connection_blocking(account)
                    })
                    .await
                    .unwrap_or_else(|_| ConnTest {
                        incoming: Err("test could not run".into()),
                        smtp: Err("test could not run".into()),
                    });
                    AccountsCmd::Test(r)
                });
            }

            AccountsInput::ProviderChanged => {
                // Editing a GOA account: the provider dropdown is hidden and the
                // connection section deliberately not shown — but filling the
                // editor sets the dropdown, whose notify lands here right after
                // EditAccount and would undo that. GNOME owns those fields;
                // leave them hidden.
                let editing_goa = self
                    .editing
                    .and_then(|i| self.accounts.get(i))
                    .is_some_and(|a| a.goa_id.is_some());
                if !editing_goa {
                    self.apply_provider(widgets);
                }
            }

            AccountsInput::OAuthSignIn => {
                let settings = self.oauth_settings_from_form(widgets);
                if settings.client_id.trim().is_empty()
                    || settings.auth_url.is_empty()
                    || settings.token_url.is_empty()
                {
                    widgets.oauth_status.set_visible(true);
                    widgets.oauth_status.set_css_classes(&["error"]);
                    widgets
                        .oauth_status
                        .set_label(&i18n("Enter a client ID (and endpoints for a custom provider) first"));
                    return;
                }
                widgets.oauth_signin_btn.set_sensitive(false);
                widgets.oauth_status.set_visible(true);
                widgets.oauth_status.set_css_classes(&["dim-label"]);
                widgets
                    .oauth_status
                    .set_label(&i18n("Opening browser… complete sign-in there."));
                sender.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        crate::oauth::run_flow(&settings).map(|f| f.refresh_token)
                    })
                    .await
                    .unwrap_or_else(|_| Err("sign-in task failed".into()));
                    AccountsCmd::OAuth(r)
                });
            }

            AccountsInput::OpenOnlineAccounts => open_online_accounts(),

            AccountsInput::Save => {
                // Pull the signature HTML out of the editor first (async), then
                // finish saving in SaveWithSig.
                let s = sender.clone();
                self.sig_editor(widgets)
                    .extract_html(move |html| s.input(AccountsInput::SaveWithSig(html)));
            }

            AccountsInput::SetFolderChoices(map) => {
                self.folders_by_email = map;
            }

            AccountsInput::SignatureEditSource => {
                let s = sender.clone();
                self.sig_editor(widgets)
                    .extract_html(move |html| s.input(AccountsInput::SignatureSourceLoaded(html)));
            }
            AccountsInput::SignatureSourceLoaded(html) => {
                let host = root.root().and_downcast::<gtk::Window>();
                let dialog = adw::MessageDialog::new(
                    host.as_ref(),
                    Some(i18n("Signature HTML").as_str()),
                    Some(
                        i18n("Paste or edit the signature's HTML. Scripts and style sheets are \
                              removed; inline styles, tables and images are kept.")
                            .as_str(),
                    ),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("apply", &i18n("Apply"));
                dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("apply"));
                let view = gtk::TextView::new();
                view.set_monospace(true);
                view.set_wrap_mode(gtk::WrapMode::WordChar);
                view.set_top_margin(8);
                view.set_bottom_margin(8);
                view.set_left_margin(8);
                view.set_right_margin(8);
                view.buffer().set_text(&html);
                let scroller = gtk::ScrolledWindow::new();
                scroller.set_child(Some(&view));
                scroller.set_size_request(620, 320);
                scroller.add_css_class("card");
                dialog.set_extra_child(Some(&scroller));
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "apply" {
                        let b = view.buffer();
                        let text = b.text(&b.start_iter(), &b.end_iter(), true).to_string();
                        s.input(AccountsInput::SignatureApplySource(text));
                    }
                });
                dialog.present();
            }
            AccountsInput::SignatureApplySource(source) => {
                self.sig_editor(widgets).set_html(&rich_editor::signature_from_source(&source, None));
            }
            AccountsInput::SignatureImport => {
                let dialog = gtk::FileDialog::new();
                dialog.set_title(&i18n("Import Signature"));
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(&i18n("HTML and text files")));
                filter.add_mime_type("text/html");
                filter.add_mime_type("text/plain");
                filter.add_suffix("html");
                filter.add_suffix("htm");
                filter.add_suffix("txt");
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));
                let parent = root.root().and_downcast::<gtk::Window>();
                let s = sender.input_sender().clone();
                dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    if let Some(path) = res.ok().and_then(|f| f.path()) {
                        let _ = s.send(AccountsInput::SignatureImportFile(path));
                    }
                });
            }
            AccountsInput::SignatureImportFile(path) => match std::fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    self.sig_editor(widgets)
                        .set_html(&rich_editor::signature_from_source(&text, path.parent()));
                }
                Err(e) => {
                    let host = root.root().and_downcast::<gtk::Window>();
                    let dialog = adw::MessageDialog::new(
                        host.as_ref(),
                        Some(i18n("Could Not Read the File").as_str()),
                        Some(&format!("{}\n{e}", path.display())),
                    );
                    dialog.add_response("ok", &i18n("OK"));
                    dialog.present();
                }
            },

            AccountsInput::SaveWithSig(sig_html) => {
                widgets.host_row.remove_css_class("error");
                let mut account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
                account.aliases = self.alias_edits.clone();
                account.folder_roles = self.read_folder_roles(widgets);
                account.sent_copy_path = self.read_sent_copy_path(widgets);
                account.server_saves_sent = widgets.server_saves_row.is_active();
                let sig = sig_html.trim();
                account.signature = if signature_is_empty(sig) {
                    None
                } else {
                    Some(sig_html.clone())
                };
                account.signature_html = true;

                // Editing preserves the enabled state; GOA accounts keep their
                // (GOA-driven) OAuth mechanism regardless of the Authentication combo.
                let editing_orig = self.editing.and_then(|i| self.accounts.get(i)).cloned();
                if let Some(orig) = &editing_orig {
                    account.enabled = orig.enabled;
                    account.goa_id = orig.goa_id.clone();
                    account.goa_mail_disabled = orig.goa_mail_disabled;
                    account.goa_enabled_before_mail_disabled =
                        orig.goa_enabled_before_mail_disabled;
                    if orig.goa_id.is_some() {
                        account.oauth = orig.oauth;
                        account.oauth_settings = orig.oauth_settings.clone();
                        // GNOME Online Accounts is the source of truth for these;
                        // Hylki keeps only what it owns (display name, signature,
                        // colour, emoji, label).
                        account.email = orig.email.clone();
                        account.protocol = orig.protocol;
                        account.imap_host = orig.imap_host.clone();
                        account.imap_port = orig.imap_port;
                        account.smtp_host = orig.smtp_host.clone();
                        account.smtp_port = orig.smtp_port;
                        account.username = orig.username.clone();
                        account.password = orig.password.clone();
                        account.smtp_separate = orig.smtp_separate;
                        account.smtp_username = orig.smtp_username.clone();
                        account.smtp_password = orig.smtp_password.clone();
                    }
                }

                // Native account: authentication comes from the provider dropdown.
                let is_oauth = provider_at(widgets.provider_row.selected()).is_oauth();
                if account.goa_id.is_none() {
                    if is_oauth {
                        account.oauth = true;
                        account.oauth_settings = Some(self.oauth_settings_from_form(widgets));
                        if account.username.trim().is_empty() {
                            account.username = account.email.clone();
                        }
                        // A fresh sign-in supplies a refresh token; otherwise keep
                        // the one already in the keyring (edit without re-signing).
                        if let Some(rt) = self.pending_oauth_refresh.clone() {
                            account.oauth_refresh = rt;
                        }
                    } else {
                        account.oauth = false;
                        account.oauth_settings = None;
                    }
                }

                // Validation.
                let oauth_ready = if account.oauth && account.goa_id.is_none() {
                    let has_client = account
                        .oauth_settings
                        .as_ref()
                        .is_some_and(|s| !s.client_id.trim().is_empty());
                    let signed_in = self.pending_oauth_refresh.is_some()
                        || editing_orig.as_ref().is_some_and(|o| o.oauth);
                    if !has_client || !signed_in {
                        widgets.oauth_status.set_visible(true);
                        widgets.oauth_status.set_css_classes(&["error"]);
                        widgets
                            .oauth_status
                            .set_label(&i18n("Enter a client ID and sign in before saving"));
                    }
                    has_client && signed_in
                } else {
                    true
                };
                // Saved before the keyring answered, or with the field left
                // as it was: the stored secrets stand.
                if AccountSecrets::needed(&account) {
                    AccountSecrets::load(&account).apply(&mut account);
                }
                let password_ok = account.oauth || !account.password.is_empty();
                // A GOA account's connection fields were all restored from the
                // original above (GNOME owns them; a Graph account rightly has
                // no IMAP host at all) — validating them would only block the
                // fields Hylki does own: label, signature, colour, aliases.
                let is_goa_edit = account.goa_id.is_some();
                if !is_goa_edit
                    && (account.imap_host.is_empty()
                        || account.username.is_empty()
                        || !password_ok
                        || !oauth_ready
                        || (account.smtp_separate
                            && (account.smtp_username.is_empty()
                                || account.smtp_password.is_empty())))
                {
                    widgets.host_row.add_css_class("error");
                    return;
                }
                self.pending_oauth_refresh = None;

                let original_email = self
                    .editing
                    .and_then(|i| self.accounts.get(i))
                    .map(|a| a.email.clone());
                match self.editing {
                    Some(i) if i < self.accounts.len() => self.accounts[i] = account.clone(),
                    _ => self.accounts.push(account.clone()),
                }
                self.rebuild_account_list(&widgets.accounts_list, &sender);
                let _ = sender.output(AccountsOutput::Saved {
                    original_email,
                    account: Box::new(account),
                });
                widgets.nav.pop();
            }

            AccountsInput::RemoveCurrent => {
                // Confirm before this destructive, keyring-clearing action.
                let Some(i) = self.editing else { return };
                let Some(account) = self.accounts.get(i) else { return };
                let name = if account.name.trim().is_empty() {
                    account.email.clone()
                } else {
                    account.name.clone()
                };
                let body = if account.goa_id.is_some() {
                    format!(
                        "Remove {name} from Hylki? It stays in GNOME Online Accounts \
                         and can be imported again from the list below. Mail on the \
                         server is not affected."
                    )
                } else {
                    format!(
                        "Remove {name} from Hylki? Its saved password is deleted from \
                         the keyring. Mail on the server is not affected."
                    )
                };
                // The panel is embedded — dialogs parent to whatever window
                // it currently sits in (the combined settings window).
                let host = root.root().and_downcast::<gtk::Window>();
                let dialog =
                    adw::MessageDialog::new(host.as_ref(), Some(i18n("Remove Account?").as_str()), Some(&body));
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("remove", &i18n("Remove"));
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "remove" {
                        s.input(AccountsInput::ConfirmRemove);
                    }
                });
                dialog.present();
            }
            AccountsInput::ConfirmRemove => {
                if let Some(i) = self.editing {
                    if i < self.accounts.len() {
                        let email = self.accounts[i].email.clone();
                        self.accounts.remove(i);
                        self.rebuild_account_list(&widgets.accounts_list, &sender);
                        let _ = sender.output(AccountsOutput::Removed { email });
                        // A removed GOA import returns to the list below (and a
                        // repaired-away broken entry re-offers its GOA account).
                        self.goa = importable_goa_accounts(&self.accounts);
                        self.rebuild_goa_list(&widgets.goa_list, &sender);
                        widgets.goa_group.set_visible(!self.goa.is_empty());
                    }
                }
                widgets.nav.pop();
            }

            AccountsInput::AliasAdd => {
                self.alias_editing = None;
                self.open_alias_dialog(root, &AliasConfig::default(), &sender);
            }

            AccountsInput::AliasEdit(i) => {
                let Some(alias) = self.alias_edits.get(i).cloned() else {
                    return;
                };
                self.alias_editing = Some(i);
                self.open_alias_dialog(root, &alias, &sender);
            }

            AccountsInput::AliasRemove(i) => {
                if i < self.alias_edits.len() {
                    // The keyring entry (if the alias had its own SMTP) is
                    // dropped on Save, when the removal actually takes effect.
                    self.alias_edits.remove(i);
                    self.rebuild_alias_list(&widgets.aliases_list, &sender);
                }
            }

            AccountsInput::AliasDialogSave => {
                let Some(d) = self.alias_dialog.as_ref() else { return };
                for row in [&d.addr_row, &d.host_row, &d.user_row] {
                    row.remove_css_class("error");
                }
                d.pass_row.remove_css_class("error");

                let name = trimmed(&d.name_row);
                let addr = trimmed(&d.addr_row);
                let own_smtp = d.smtp_switch.is_active();
                let host = trimmed(&d.host_row);
                let port: u16 = trimmed(&d.port_row).parse().unwrap_or(587);
                let user = trimmed(&d.user_row);
                let pass = d.pass_row.text().to_string();

                // The address must look like one, and must not shadow the
                // account's own address or another alias — the send path picks
                // an alias's transport by matching the From address, so a
                // duplicate would make "which server sends this?" ambiguous.
                let account_email = trimmed(&widgets.email_row);
                let duplicate = addr.eq_ignore_ascii_case(&account_email)
                    || self.alias_edits.iter().enumerate().any(|(i, a)| {
                        Some(i) != self.alias_editing
                            && a.address().eq_ignore_ascii_case(&addr)
                    });
                let mut bad = false;
                if addr.is_empty() || !addr.contains('@') || duplicate {
                    d.addr_row.add_css_class("error");
                    bad = true;
                }
                if own_smtp {
                    if host.is_empty() {
                        d.host_row.add_css_class("error");
                        bad = true;
                    }
                    if user.is_empty() {
                        d.user_row.add_css_class("error");
                        bad = true;
                    }
                    if pass.is_empty() {
                        d.pass_row.add_css_class("error");
                        bad = true;
                    }
                }
                if bad {
                    return;
                }

                let alias = AliasConfig {
                    identity: if name.is_empty() {
                        addr.clone()
                    } else {
                        format!("{name} <{addr}>")
                    },
                    smtp_host: if own_smtp { host } else { String::new() },
                    smtp_port: port,
                    smtp_username: if own_smtp { user } else { String::new() },
                    smtp_password: if own_smtp { pass } else { String::new() },
                };
                match self.alias_editing {
                    Some(i) if i < self.alias_edits.len() => self.alias_edits[i] = alias,
                    _ => self.alias_edits.push(alias),
                }
                self.alias_editing = None;
                self.close_alias_dialog();
                self.rebuild_alias_list(&widgets.aliases_list, &sender);
            }

            AccountsInput::AliasDialogTest => {
                let Some(d) = self.alias_dialog.as_ref() else { return };
                let alias = AliasConfig {
                    identity: trimmed(&d.addr_row),
                    smtp_host: trimmed(&d.host_row),
                    smtp_port: trimmed(&d.port_row).parse().unwrap_or(587),
                    smtp_username: trimmed(&d.user_row),
                    smtp_password: d.pass_row.text().to_string(),
                };
                let email = trimmed(&widgets.email_row);
                d.test_btn.set_sensitive(false);
                d.test_result.set_visible(true);
                d.test_result.set_css_classes(&["dim-label"]);
                d.test_result.set_label(&i18n("Testing…"));
                sender.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        worker::test_alias_smtp_blocking(email, alias)
                    })
                    .await
                    .unwrap_or_else(|_| Err("test could not run".into()));
                    AccountsCmd::AliasTested(r)
                });
            }

            AccountsInput::AliasDialogClosed => {
                // The notification is queued behind the close, so by the time it
                // arrives a replacement dialog may already be open — only clear
                // state when the dialog we track is really the one that closed.
                if self.alias_dialog.as_ref().is_none_or(|d| !d.window.is_visible()) {
                    self.alias_dialog = None;
                    self.alias_editing = None;
                }
            }

            AccountsInput::AddSenderText(text) => {
                let addr = text.trim().to_lowercase();
                if !addr.is_empty() && !self.sender_addrs.contains(&addr) {
                    self.sender_addrs.push(addr.clone());
                    self.senders.guard().push_back(addr.clone());
                    self.senders.widget().set_visible(true);
                    let _ = sender.output(AccountsOutput::AddSender(addr));
                }
            }
            AccountsInput::RemoveSenderRow(addr) => {
                if let Some(pos) = self.sender_addrs.iter().position(|a| *a == addr) {
                    self.sender_addrs.remove(pos);
                    self.senders.guard().remove(pos);
                    self.senders.widget().set_visible(!self.sender_addrs.is_empty());
                    let _ = sender.output(AccountsOutput::RemoveSender(addr));
                }
            }
            AccountsInput::AddBlacklistText(text) => {
                let addr = text.trim().to_lowercase();
                if !addr.is_empty() && !self.blacklist_addrs.contains(&addr) {
                    self.blacklist_addrs.push(addr.clone());
                    self.blacklist.guard().push_back(addr.clone());
                    self.blacklist.widget().set_visible(true);
                    let _ = sender.output(AccountsOutput::AddBlacklist(addr));
                }
            }
            AccountsInput::RemoveBlacklistRow(addr) => {
                if let Some(pos) = self.blacklist_addrs.iter().position(|a| *a == addr) {
                    self.blacklist_addrs.remove(pos);
                    self.blacklist.guard().remove(pos);
                    self.blacklist.widget().set_visible(!self.blacklist_addrs.is_empty());
                    let _ = sender.output(AccountsOutput::RemoveBlacklist(addr));
                }
            }
            AccountsInput::AddFilter => {
                self.open_filter_page(&widgets.nav, &sender, None);
            }
            AccountsInput::ApplyFilters => {
                if !self.filters_applying {
                    self.set_filters_applying(true);
                    let _ = sender.output(AccountsOutput::ApplyFilters);
                }
            }
            AccountsInput::FiltersApplying(on) => self.set_filters_applying(on),
            AccountsInput::EditFilter(i) => {
                if let Some(rule) = self.filter_rules.get(i).cloned() {
                    self.open_filter_page(&widgets.nav, &sender, Some((i, rule)));
                }
            }
            AccountsInput::FilterEdited(i, rule) => {
                if let Some(slot) = self.filter_rules.get_mut(i) {
                    if *slot != rule {
                        *slot = rule;
                        self.rebuild_filter_rows(&sender);
                        let _ = sender
                            .output(AccountsOutput::SetFilters(self.filter_rules.clone()));
                    }
                }
            }
            AccountsInput::RemoveFilter(i) => {
                if i < self.filter_rules.len() {
                    self.filter_rules.remove(i);
                    self.rebuild_filter_rows(&sender);
                    let _ =
                        sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
                }
            }
            AccountsInput::FilterAdded(rule) => {
                self.filter_rules.push(rule);
                self.rebuild_filter_rows(&sender);
                let _ = sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
            }
            AccountsInput::AddTag => self.open_tag_page(&widgets.nav, &sender, None),
            AccountsInput::EditTag(i) => {
                if let Some(tag) = self.tags.get(i).cloned() {
                    self.open_tag_page(&widgets.nav, &sender, Some((i, tag)));
                }
            }
            AccountsInput::RemoveTag(i) => {
                if i < self.tags.len() {
                    self.tags.remove(i);
                    self.rebuild_tag_rows(&sender);
                    // The rules name tags by keyword; a rule whose tag is gone
                    // shows the bare keyword until it is edited.
                    self.rebuild_filter_rows(&sender);
                    let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                }
            }
            AccountsInput::MoveTag { from, to } => {
                if from < self.tags.len() && from != to {
                    let tag = self.tags.remove(from);
                    let to = to.min(self.tags.len());
                    self.tags.insert(to, tag);
                    self.rebuild_tag_rows(&sender);
                    let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                }
            }
            AccountsInput::TagAdded(tag) => {
                self.tags.push(tag);
                self.rebuild_tag_rows(&sender);
                let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
            }
            AccountsInput::TagEdited(i, tag) => {
                if let Some(slot) = self.tags.get_mut(i) {
                    if *slot != tag {
                        *slot = tag;
                        self.rebuild_tag_rows(&sender);
                        self.rebuild_filter_rows(&sender);
                        let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                    }
                }
            }
            AccountsInput::FindTags => {
                if !self.tag_scanning {
                    self.set_tag_scanning(true);
                    let _ = sender.output(AccountsOutput::FindTags);
                }
            }
            AccountsInput::TagScanning(on) => self.set_tag_scanning(on),
            AccountsInput::TagFindings(found) => {
                self.set_tag_scanning(false);
                let parent = root.root().and_downcast::<gtk::Window>();
                self.show_tag_findings(parent.as_ref(), found, &sender);
            }
            AccountsInput::ImportTags(tags) => {
                let mut added = 0;
                for t in tags {
                    if !self.tags.iter().any(|x| x.keyword.eq_ignore_ascii_case(&t.keyword)) {
                        self.tags.push(t);
                        added += 1;
                    }
                }
                if added > 0 {
                    self.rebuild_tag_rows(&sender);
                    self.rebuild_filter_rows(&sender);
                    let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                }
            }
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        result: AccountsCmd,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match result {
            AccountsCmd::OwnGravatar { email, outcome } => {
                // Cached for every other circle in the app as well; the
                // preview only redraws while that address is still in the
                // editor, and the accounts list redraws its circles so one
                // that landed after the list was built shows there too.
                let found = crate::avatar::cache_own_gravatar(&email, outcome);
                if found && trimmed(&widgets.email_row).eq_ignore_ascii_case(&email) {
                    self.refresh_preview(widgets);
                }
                if found {
                    self.rebuild_account_list(&widgets.accounts_list, &sender);
                }
            }

            AccountsCmd::Secrets(secrets) => {
                // Still editing that account: fill in what the user hasn't
                // typed over meanwhile.
                let Some(i) = self.editing else { return };
                if self.accounts.get(i).is_none_or(|a| a.email != secrets.email) {
                    return;
                }
                // The passwords arrive after the editor opened, so they would
                // read as an edit the user never made. Re-take the seed below
                // when nothing has actually been typed yet.
                let untouched = !self.editor_touched(widgets);
                if let Some(acc) = self.accounts.get_mut(i) {
                    secrets.apply(acc);
                }
                if widgets.pass_row.text().is_empty() {
                    widgets.pass_row.set_text(&secrets.password);
                }
                if widgets.smtp_pass_row.text().is_empty() {
                    widgets.smtp_pass_row.set_text(&secrets.smtp_password);
                }
                for al in self.alias_edits.iter_mut() {
                    if al.smtp_password.is_empty() {
                        if let Some((_, pw)) = secrets.aliases.iter().find(|(a, _)| *a == al.address()) {
                            al.smtp_password = pw.clone();
                        }
                    }
                }
                if untouched {
                    self.editor_seed = Some(self.editor_fingerprint(widgets));
                }
            }
            AccountsCmd::Test(result) => {
                let line = |label: &str, r: &Result<(), String>| match r {
                    Ok(()) => format!("✓ {label}: connected"),
                    Err(e) => format!("✗ {label}: {e}"),
                };
                let incoming_label =
                    if widgets.protocol_row.selected() == 1 { "POP3" } else { "IMAP" };
                let text = format!(
                    "{}\n{}",
                    line(incoming_label, &result.incoming),
                    line("SMTP", &result.smtp),
                );
                let class = if result.incoming.is_ok() && result.smtp.is_ok() {
                    "success"
                } else {
                    "error"
                };
                widgets.test_result.set_label(&text);
                widgets.test_result.set_css_classes(&[class]);
                widgets.test_btn.set_sensitive(true);
            }
            AccountsCmd::OAuth(result) => {
                widgets.oauth_signin_btn.set_sensitive(true);
                widgets.oauth_status.set_visible(true);
                match result {
                    Ok(refresh) => {
                        self.pending_oauth_refresh = Some(refresh);
                        widgets.oauth_status.set_css_classes(&["success"]);
                        widgets.oauth_status.set_label(&i18n("✓ Signed in — save the account to finish"));
                    }
                    Err(e) => {
                        widgets.oauth_status.set_css_classes(&["error"]);
                        widgets.oauth_status.set_label(&i18n_f("Sign-in failed: {e}", &[("e", &(e).to_string())]));
                    }
                }
            }
            AccountsCmd::AliasTested(result) => {
                let Some(d) = self.alias_dialog.as_ref() else { return };
                d.test_btn.set_sensitive(true);
                d.test_result.set_visible(true);
                match result {
                    Ok(()) => {
                        d.test_result.set_css_classes(&["success"]);
                        d.test_result.set_label(&i18n("✓ SMTP: connected"));
                    }
                    Err(e) => {
                        d.test_result.set_css_classes(&["error"]);
                        d.test_result.set_label(&i18n_f("✗ SMTP: {e}", &[("e", &(e).to_string())]));
                    }
                }
            }
        }
    }
}

impl AccountsWindow {
    /// Rebuild the editor's send-as alias list from `alias_edits` (#34).
    /// The signature editor, created (and mounted in its holder) on first
    /// use: a WebKit view is the slowest thing the panel would otherwise
    /// build, for a field most visits never reach.
    fn sig_editor(&mut self, widgets: &AccountsWindowWidgets) -> &RichEditor {
        if self.sig_editor.is_none() {
            let editor = RichEditor::new("");
            widgets.sig_holder.append(&editor.widget);
            self.sig_editor = Some(editor);
        }
        self.sig_editor.as_ref().expect("created above")
    }

    fn rebuild_alias_list(&self, list: &gtk::ListBox, sender: &ComponentSender<Self>) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        // An empty boxed-list draws as a bare frame; hide it until there is a row.
        list.set_visible(!self.alias_edits.is_empty());

        for (i, alias) in self.alias_edits.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            row.set_title(&gtk::glib::markup_escape_text(&alias.identity));
            row.set_subtitle(&gtk::glib::markup_escape_text(&if alias.has_own_smtp() {
                i18n_f("Sends through {host}", &[("host", &alias.smtp_host)])
            } else {
                i18n("Sends through this account")
            }));

            // A dim pencil says "activate to edit"; the trash button removes.
            let edit = gtk::Image::from_icon_name("co.hyprlab.Hylki-document-edit-symbolic");
            edit.add_css_class("dim-label");
            row.add_suffix(&edit);

            let remove = gtk::Button::from_icon_name("co.hyprlab.Hylki-user-trash-symbolic");
            remove.set_valign(gtk::Align::Center);
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some(i18n("Remove this alias").as_str()));
            let ri = sender.input_sender().clone();
            remove.connect_clicked(move |_| {
                let _ = ri.send(AccountsInput::AliasRemove(i));
            });
            row.add_suffix(&remove);

            list.append(&row);
        }
    }

    /// Close the alias dialog, if one is open.
    fn close_alias_dialog(&mut self) {
        if let Some(d) = self.alias_dialog.take() {
            d.window.close();
        }
    }

    /// Open the modal alias editor (#34), prefilled from `alias`.
    fn open_alias_dialog(
        &mut self,
        root: &adw::Bin,
        alias: &AliasConfig,
        sender: &ComponentSender<Self>,
    ) {
        self.close_alias_dialog();

        let window = adw::Window::builder()
            .modal(true)
            .default_width(440)
            .title(if self.alias_editing.is_some() { "Edit Alias" } else { "Add Alias" })
            .build();
        // Parent to whatever window the embedded panel currently sits in.
        window.set_transient_for(root.root().and_downcast::<gtk::Window>().as_ref());

        let cancel = gtk::Button::with_label(&i18n("Cancel"));
        let save = gtk::Button::with_label(&i18n("Save"));
        save.add_css_class("suggested-action");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);

        let (name, addr) = split_identity(&alias.identity);
        let identity_group = adw::PreferencesGroup::new();
        identity_group.set_description(Some(
            "The composer's From menu offers this address, and replies to \
             mail sent to it answer from it.",
        ));
        let name_row = adw::EntryRow::builder().title(&i18n("Display name (optional)")).build();
        name_row.set_text(&name);
        let addr_row = adw::EntryRow::builder().title(&i18n("Email address")).build();
        addr_row.set_text(&addr);
        identity_group.add(&name_row);
        identity_group.add(&addr_row);

        let smtp_group = adw::PreferencesGroup::new();
        smtp_group.set_title(&i18n("Sending"));
        let smtp_switch = adw::SwitchRow::builder()
            .title(&i18n("Own SMTP server"))
            .subtitle(
                "Send through the alias's own mail provider, with its own \
                 sign-in — instead of this account's server. Needed when the \
                 account's provider (e.g. Gmail) rewrites the sender address.",
            )
            .build();
        smtp_switch.set_active(alias.has_own_smtp());
        let host_row = adw::EntryRow::builder().title(&i18n("SMTP server")).build();
        host_row.set_text(&alias.smtp_host);
        let port_row = adw::EntryRow::builder().title(&i18n("SMTP port")).build();
        port_row.set_text(&alias.smtp_port.to_string());
        let user_row = adw::EntryRow::builder().title(&i18n("SMTP username")).build();
        user_row.set_text(&alias.smtp_username);
        let pass_row = adw::PasswordEntryRow::builder().title(&i18n("SMTP password")).build();
        pass_row.set_text(&alias.smtp_password);
        smtp_group.add(&smtp_switch);
        smtp_group.add(&host_row);
        smtp_group.add(&port_row);
        smtp_group.add(&user_row);
        smtp_group.add(&pass_row);

        let test_btn = gtk::Button::with_label(&i18n("Test SMTP"));
        test_btn.set_halign(gtk::Align::Start);
        let test_result = gtk::Label::new(None);
        test_result.set_visible(false);
        test_result.set_halign(gtk::Align::Start);
        test_result.set_xalign(0.0);
        test_result.set_wrap(true);
        let test_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        test_box.append(&test_btn);
        test_box.append(&test_result);

        // The SMTP fields (and their test) only apply with the switch on.
        for target in [
            host_row.upcast_ref::<gtk::Widget>(),
            port_row.upcast_ref(),
            user_row.upcast_ref(),
            pass_row.upcast_ref(),
            test_box.upcast_ref(),
        ] {
            smtp_switch
                .bind_property("active", target, "visible")
                .sync_create()
                .build();
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(24);
        content.set_margin_end(24);
        content.append(&identity_group);
        content.append(&smtp_group);
        content.append(&test_box);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(640)
            .child(&content)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));
        window.set_content(Some(&toolbar));

        let w = window.clone();
        cancel.connect_clicked(move |_| w.close());
        let s = sender.input_sender().clone();
        save.connect_clicked(move |_| {
            let _ = s.send(AccountsInput::AliasDialogSave);
        });
        let s = sender.input_sender().clone();
        test_btn.connect_clicked(move |_| {
            let _ = s.send(AccountsInput::AliasDialogTest);
        });
        let s = sender.input_sender().clone();
        window.connect_close_request(move |_| {
            let _ = s.send(AccountsInput::AliasDialogClosed);
            gtk::glib::Propagation::Proceed
        });

        window.present();
        self.alias_dialog = Some(AliasDialog {
            window,
            name_row,
            addr_row,
            smtp_switch,
            host_row,
            port_row,
            user_row,
            pass_row,
            test_btn,
            test_result,
        });
    }

    /// Rebuild the draggable account list.
    /// Fill the editor's Special Folders combos (#82) for `acc` (None = a new
    /// account, whose folders aren't known yet): "Automatic" plus the account's
    /// live folder list, with any saved assignment selected.
    fn populate_folder_combos(&mut self, widgets: &AccountsWindowWidgets, acc: Option<&AccountConfig>) {
        let all: Vec<(String, String)> = acc
            .and_then(|a| self.folders_by_email.get(&a.email))
            .cloned()
            .unwrap_or_default();
        // The Inbox is never offered as a role (#136): giving it one took
        // its own role away, and the account lost its inbox.
        let choices: Vec<(String, String)> = all
            .iter()
            .filter(|(path, _)| !path.eq_ignore_ascii_case("INBOX"))
            .cloned()
            .collect();
        self.populate_sent_copy_combo(widgets, acc, &all);
        let mut labels: Vec<&str> = vec!["Automatic"];
        labels.extend(choices.iter().map(|(_, display)| display.as_str()));
        self.folder_paths = choices.iter().map(|(path, _)| path.clone()).collect();
        for (role, row) in [
            ("sent", &widgets.folder_sent_row),
            ("drafts", &widgets.folder_drafts_row),
            ("trash", &widgets.folder_trash_row),
            ("junk", &widgets.folder_junk_row),
            ("archive", &widgets.folder_archive_row),
        ] {
            row.set_model(Some(&gtk::StringList::new(&labels)));
            row.set_list_factory(Some(&non_ellipsizing_factory()));
            let selected = acc
                .and_then(|a| a.folder_roles.get(role))
                .and_then(|path| self.folder_paths.iter().position(|p| p == path))
                .map(|i| i as u32 + 1)
                .unwrap_or(0);
            row.set_selected(selected);
        }
    }

    /// Fill the "Save a copy of sent mail in" combo (#199): "Disabled" plus
    /// every one of the account's folders, with the saved choice selected. The
    /// Inbox is listed here — filing a copy somewhere does not re-label it.
    fn populate_sent_copy_combo(
        &mut self,
        widgets: &AccountsWindowWidgets,
        acc: Option<&AccountConfig>,
        all: &[(String, String)],
    ) {
        // "Disabled", not "Automatic": the rows around it auto-detect a folder
        // from the server's markings, and this one detects nothing. It is an
        // override that is either set or not, and unset means the Sent folder
        // above keeps taking the copies as it always has.
        let mut labels: Vec<&str> = vec!["Disabled"];
        labels.extend(all.iter().map(|(_, display)| display.as_str()));
        self.sent_copy_paths = all.iter().map(|(path, _)| path.clone()).collect();
        let row = &widgets.folder_sent_copy_row;
        row.set_model(Some(&gtk::StringList::new(&labels)));
        row.set_list_factory(Some(&non_ellipsizing_factory()));
        let selected = acc
            .and_then(|a| a.sent_copy_path.as_ref())
            .and_then(|path| self.sent_copy_paths.iter().position(|p| p == path))
            .map(|i| i as u32 + 1)
            .unwrap_or(0);
        row.set_selected(selected);
        // Two backends file the copy themselves, or not at all, and ignore
        // this setting — say so rather than offer a choice that does nothing.
        let unsupported = match acc.map(|a| a.protocol) {
            Some(Protocol::Graph) => Some(i18n(
                "Microsoft 365 files its own copy in Sent Items; this cannot be changed.",
            )),
            Some(Protocol::Pop3) => Some(i18n(
                "POP3 accounts have no server folders to save a copy in.",
            )),
            _ => None,
        };
        // The switch above may already have taken this row out of play.
        let server_saves = acc.is_some_and(|a| a.server_saves_sent);
        widgets.server_saves_row.set_active(server_saves);
        row.set_sensitive(unsupported.is_none() && !server_saves);
        row.set_subtitle(&unsupported.unwrap_or_else(|| i18n(SENT_COPY_HINT)));
    }

    /// The "Save a copy of sent mail in" combo's current choice: a folder path,
    /// or `None` when disabled, which leaves the copy to the Sent role.
    fn read_sent_copy_path(&self, widgets: &AccountsWindowWidgets) -> Option<String> {
        // Nothing is known about this account's folders yet (it has never
        // connected, or is switched off), so the combo holds only "Sent
        // folder" — which is not the user clearing the setting. Keep what
        // was saved rather than silently dropping it on an unrelated edit.
        if self.sent_copy_paths.is_empty() {
            return self
                .editing
                .and_then(|i| self.accounts.get(i))
                .and_then(|a| a.sent_copy_path.clone());
        }
        let sel = widgets.folder_sent_copy_row.selected();
        if sel == 0 {
            return None;
        }
        self.sent_copy_paths.get(sel as usize - 1).cloned()
    }

    /// The Special Folders combos' current assignments: role → folder path,
    /// omitting everything left on Automatic.
    fn read_folder_roles(
        &self,
        widgets: &AccountsWindowWidgets,
    ) -> std::collections::BTreeMap<String, String> {
        let mut roles = std::collections::BTreeMap::new();
        for (role, row) in [
            ("sent", &widgets.folder_sent_row),
            ("drafts", &widgets.folder_drafts_row),
            ("trash", &widgets.folder_trash_row),
            ("junk", &widgets.folder_junk_row),
            ("archive", &widgets.folder_archive_row),
        ] {
            let sel = row.selected();
            if sel > 0 {
                if let Some(path) = self.folder_paths.get(sel as usize - 1) {
                    roles.insert(role.to_string(), path.clone());
                }
            }
        }
        roles
    }

    fn rebuild_account_list(&self, list: &gtk::ListBox, sender: &ComponentSender<Self>) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let mut css = String::new();
        // The provider mark and the source badge each get a column of their
        // own, held to one width down the whole list. Both vary: the marks
        // are logos of different aspect (Gmail's M is wider than an envelope),
        // and "GOA" is narrower than "Hylki" (and the pair differs again in
        // each language). Left to themselves they pushed each other sideways
        // and neither read as a column.
        let mark_widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let badge_widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        // A slot keeps its child at its natural size and centred; the size
        // group holds the slot, not the child, so a logo is never stretched
        // and a badge's pill still hugs its text.
        let slot = |child: &gtk::Widget, group: &gtk::SizeGroup| {
            let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            slot.set_valign(gtk::Align::Center);
            child.set_halign(gtk::Align::Center);
            child.set_hexpand(true);
            slot.append(child);
            // The child expands to fill the slot so it lands centred — but an
            // expanding child makes its parent expand too, and an expanding
            // slot would take a share of the row's spare width and defeat the
            // whole point. Pin the slot's own answer to "no".
            slot.set_hexpand(false);
            group.add_widget(&slot);
            slot
        };
        for (pos, acc) in self.accounts.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);

            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("account-list-row");

            let handle = gtk::Image::from_icon_name("co.hyprlab.Hylki-list-drag-handle-symbolic");
            handle.add_css_class("dim-label");
            hbox.append(&handle);

            // The account's circle, as the sidebar draws it: its colour (or
            // the palette accent it would get), and its picture, emoji or
            // initials — the label first, then the name, then the address.
            let account_id = pos as u32 + 1;
            let color = acc
                .color
                .clone()
                .unwrap_or_else(|| crate::worker::accent_for(account_id).to_string());
            css.push_str(&format!(
                ".acct-list-color-{account_id} {{ background-color: {color}; }}\n"
            ));
            let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            circle.add_css_class("account-circle");
            circle.add_css_class(&format!("acct-list-color-{account_id}"));
            circle.set_valign(gtk::Align::Center);
            circle.set_halign(gtk::Align::Center);
            circle.set_hexpand(false);
            circle.set_size_request(30, 30);
            let shown = match acc.label.as_deref().map(str::trim) {
                Some(l) if !l.is_empty() && l != acc.email => l.to_string(),
                _ => display_name(acc),
            };
            let picture = acc
                .avatar
                .as_deref()
                .and_then(crate::config::avatar_path)
                .filter(|p| p.exists());
            // The Gravatar this address asked for and has (#189) comes first,
            // as in the sidebar circle and the editor preview.
            let gravatar = acc.gravatar.then(|| crate::avatar::own_gravatar(&acc.email)).flatten();
            let glyph: gtk::Widget = match (&gravatar, &picture, acc.emoji.as_deref()) {
                (Some(texture), ..) => {
                    circle.set_overflow(gtk::Overflow::Hidden);
                    crate::ui::initials::picture_from_texture(texture, 30).upcast()
                }
                (None, Some(path), _) => {
                    circle.set_overflow(gtk::Overflow::Hidden);
                    crate::ui::initials::avatar_picture(path, 30).upcast()
                }
                (None, None, Some(em)) if !em.is_empty() => {
                    crate::ui::initials::glyph_picture(em, &color, 0.55, 30).upcast()
                }
                _ => crate::ui::initials::glyph_picture(
                    &crate::ui::sidebar::account_initials(&shown, &acc.email),
                    &color,
                    0.47,
                    30,
                )
                .upcast(),
            };
            circle.append(&glyph);
            hbox.append(&circle);

            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.set_hexpand(true);
            vbox.set_valign(gtk::Align::Center);
            let name = gtk::Label::new(Some(&display_name(acc)));
            name.set_halign(gtk::Align::Start);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.add_css_class("account-name");
            let email = gtk::Label::new(Some(&acc.email));
            email.set_halign(gtk::Align::Start);
            email.set_ellipsize(gtk::pango::EllipsizeMode::End);
            email.add_css_class("account-email");
            vbox.append(&name);
            vbox.append(&email);
            hbox.append(&vbox);

            // The provider's mark, right of the name and address, named on
            // hover.
            let brand = brand_for_account(acc);
            let mark = crate::brand::image_or(brand, 24, crate::brand::GENERIC_MAIL);
            mark.set_tooltip_text(Some(&provider_name(brand, acc)));
            hbox.append(&slot(mark.upcast_ref(), &mark_widths));

            // Source badge: is this account from GNOME Online Accounts, or added
            // directly in Hylki?
            let from_goa = acc.goa_id.is_some();
            let badge = gtk::Label::new(Some(if from_goa { i18n("GOA") } else { i18n("Hylki") }.as_str()));
            badge.set_valign(gtk::Align::Center);
            badge.add_css_class("account-source-badge");
            if from_goa {
                badge.add_css_class("goa");
                badge.set_tooltip_text(Some(i18n("Imported GNOME Online Account").as_str()));
            } else {
                badge.set_tooltip_text(Some(i18n("Added directly in Hylki").as_str()));
            }
            hbox.append(&slot(badge.upcast_ref(), &badge_widths));

            // Enable/disable toggle. Disabled accounts stay configured but don't
            // sync or appear in the sidebar.
            let toggle = gtk::Switch::new();
            toggle.set_valign(gtk::Align::Center);
            if acc.goa_mail_disabled {
                toggle.set_tooltip_text(Some(
                    "Paused: Mail is switched off for this account in GNOME Settings",
                ));
                toggle.set_sensitive(false);
            } else {
                toggle.set_tooltip_text(Some(i18n("Enable this account").as_str()));
            }
            toggle.set_active(acc.enabled);
            let ti = sender.input_sender().clone();
            let tpos = pos;
            toggle.connect_state_set(move |_, state| {
                let _ = ti.send(AccountsInput::ToggleEnabled { index: tpos, enabled: state });
                gtk::glib::Propagation::Proceed
            });
            hbox.append(&toggle);

            let next = gtk::Image::from_icon_name("co.hyprlab.Hylki-go-next-symbolic");
            next.add_css_class("dim-label");
            hbox.append(&next);

            row.set_child(Some(&hbox));

            // Drag to reorder.
            let drag = gtk::DragSource::new();
            drag.set_actions(gtk::gdk::DragAction::MOVE);
            let from = pos as u32;
            drag.connect_prepare(move |_, _, _| {
                Some(gtk::gdk::ContentProvider::for_value(&from.to_value()))
            });
            row.add_controller(drag);

            let drop = gtk::DropTarget::new(gtk::glib::Type::U32, gtk::gdk::DragAction::MOVE);
            let to = pos;
            let input = sender.input_sender().clone();
            drop.connect_drop(move |_, value, _, _| {
                if let Ok(from) = value.get::<u32>() {
                    let _ = input.send(AccountsInput::MoveRow {
                        from: from as usize,
                        to,
                    });
                    true
                } else {
                    false
                }
            });
            row.add_controller(drop);

            list.append(&row);
        }
        self.list_css.load_from_string(&css);
    }

    /// Un-import a GNOME Online Account: drop it from Hylki (the app removes
    /// the config entry and its stored copies) and return it to the "GNOME
    /// Online Accounts" import list below. The account stays in GNOME.
    fn unimport_goa(
        &mut self,
        index: usize,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if index >= self.accounts.len() {
            return;
        }
        let email = self.accounts[index].email.clone();
        self.accounts.remove(index);
        let _ = sender.output(AccountsOutput::Removed { email });
        // Re-query GOA so the account's row reappears in the import list fresh.
        self.goa = importable_goa_accounts(&self.accounts);
        self.rebuild_account_list(&widgets.accounts_list, sender);
        self.rebuild_goa_list(&widgets.goa_list, sender);
        widgets.goa_group.set_visible(!self.goa.is_empty());
    }

    /// Populate the "GNOME Online Accounts" list with importable mail accounts.
    fn rebuild_goa_list(&self, list: &gtk::ListBox, sender: &ComponentSender<Self>) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        for (pos, g) in self.goa.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&g.email);
            row.add_prefix(&crate::brand::image_or(brand_for_goa(&g.provider), 24, crate::brand::GENERIC_MAIL));
            let mut subtitle = if g.provider.is_empty() {
                "Mail".to_string()
            } else {
                g.provider.clone()
            };
            // OAuth providers (Gmail, Microsoft 365) sign in with a token from
            // GNOME.
            if g.oauth2 && !g.password_based {
                subtitle.push_str(" · sign-in via GNOME");
            }
            row.set_subtitle(&subtitle);

            let toggle = gtk::Switch::new();
            toggle.set_valign(gtk::Align::Center);
            toggle.set_active(false);
            toggle.set_tooltip_text(Some(i18n("Use this account in Hylki").as_str()));
            let ti = sender.input_sender().clone();
            let tpos = pos;
            toggle.connect_state_set(move |_, state| {
                if state {
                    let _ = ti.send(AccountsInput::ImportGoa(tpos));
                }
                gtk::glib::Propagation::Proceed
            });
            row.add_suffix(&toggle);
            list.append(&row);
        }
    }

    /// Show/hide credential rows based on the Authentication combo, and pre-fill
    /// server settings for known OAuth providers.
    /// Adapt the editor to the selected provider: show server + credential fields
    /// for password providers, the OAuth sign-in for OAuth providers, and fill in
    /// the servers for known providers.
    fn apply_provider(&self, widgets: &AccountsWindowWidgets) {
        let p = provider_at(widgets.provider_row.selected());
        // The mark over the form: a GNOME Online Account's comes from the
        // account itself (its picker is hidden), otherwise the picker's.
        let editing_goa = self.editing.and_then(|i| self.accounts.get(i)).filter(|a| a.goa_id.is_some());
        let brand = editing_goa.map(brand_for_account).unwrap_or(p.brand);
        crate::brand::set_image(&widgets.provider_mark, brand, 56, crate::brand::GENERIC_MAIL);
        let is_password = p.is_password();
        let is_oauth = p.is_oauth();
        let is_custom = matches!(p.kind, ProviderKind::CustomOAuth);
        // Google or Microsoft with no built-in (or user-supplied) OAuth client:
        // there's nothing to sign in with, so guide the user to GNOME Online
        // Accounts instead. Microsoft always lands here since its embedded
        // client was removed (issue #36) — mail runs over GOA + Graph.
        let needs_goa = p
            .oauth_name()
            .filter(|_| matches!(p.kind, ProviderKind::Google | ProviderKind::Microsoft))
            .is_some_and(|n| crate::oauth::provider_credentials(n).0.trim().is_empty());
        // Google/Microsoft servers come from the built-in preset (hidden). Custom
        // OAuth still needs its server addresses and client details entered.
        let show_servers = is_password || is_custom;

        let hint = if p.hint.is_empty() { String::new() } else { i18n(p.hint) };
        widgets.provider_row.set_subtitle(&hint);

        // Server/credential fields (password or Custom-OAuth manual servers).
        widgets.protocol_row.set_visible(is_password);
        widgets.host_row.set_visible(show_servers);
        widgets.port_row.set_visible(show_servers);
        widgets.smtp_row.set_visible(show_servers);
        widgets.smtp_port_row.set_visible(show_servers);
        widgets.user_row.set_visible(is_password);
        widgets.pass_row.set_visible(is_password);
        widgets.smtp_separate_row.set_visible(is_password);
        widgets.test_btn.set_visible(is_password);
        if is_oauth {
            widgets.smtp_separate_row.set_active(false);
        }

        // OAuth: the user just signs in. Google with no client falls back to the
        // GNOME Online Accounts panel, which replaces the sign-in + identity fields.
        widgets.name_row.set_visible(!needs_goa);
        widgets.email_row.set_visible(!needs_goa);
        widgets.goa_hint.set_visible(needs_goa);
        widgets.oauth_signin_btn.set_visible(is_oauth && !needs_goa);
        widgets.oauth_client_id_row.set_visible(is_custom);
        widgets.oauth_secret_row.set_visible(is_custom);
        widgets.oauth_auth_url_row.set_visible(is_custom);
        widgets.oauth_token_url_row.set_visible(is_custom);
        widgets.oauth_scope_row.set_visible(is_custom);
        if !is_oauth || needs_goa {
            widgets.oauth_status.set_visible(false);
        }

        // Auto-fill IMAP/SMTP: known password providers from the preset table,
        // Google/Microsoft from the OAuth preset (filled but hidden, so the saved
        // account still carries the right servers). Manual/Custom are left alone.
        let servers = match p.kind {
            ProviderKind::Preset => Some((p.imap_host, p.imap_port, p.smtp_host, p.smtp_port)),
            ProviderKind::Google | ProviderKind::Microsoft => crate::oauth::preset(p.oauth_name().unwrap())
                .map(|o| (o.imap_host, o.imap_port, o.smtp_host, o.smtp_port)),
            ProviderKind::Manual | ProviderKind::CustomOAuth => None,
        };
        if let Some((ih, ip, sh, sp)) = servers {
            widgets.protocol_row.set_selected(0); // IMAP
            widgets.host_row.set_text(ih);
            widgets.port_row.set_text(&ip.to_string());
            widgets.smtp_row.set_text(sh);
            widgets.smtp_port_row.set_text(&sp.to_string());
        }
    }

    /// Build the OAuth client config from the form. Google/Microsoft use built-in
    /// endpoints + credentials; "Custom OAuth" uses the user-entered fields.
    fn oauth_settings_from_form(&self, widgets: &AccountsWindowWidgets) -> OAuthSettings {
        let provider = provider_at(widgets.provider_row.selected()).oauth_name();
        if let Some(name) = provider {
            let p = crate::oauth::preset(name).unwrap();
            let (client_id, client_secret) = crate::oauth::provider_credentials(name);
            OAuthSettings {
                auth_url: p.auth_url.to_string(),
                token_url: p.token_url.to_string(),
                client_id,
                client_secret,
                scopes: p.scopes.to_string(),
            }
        } else {
            OAuthSettings {
                auth_url: trimmed(&widgets.oauth_auth_url_row),
                token_url: trimmed(&widgets.oauth_token_url_row),
                client_id: trimmed(&widgets.oauth_client_id_row),
                client_secret: widgets.oauth_secret_row.text().to_string(),
                scopes: trimmed(&widgets.oauth_scope_row),
            }
        }
    }
}

/// A list-item factory whose labels never ellipsize, so a `ComboRow` popup grows
/// to fit its longest option instead of truncating it.
/// Open GNOME Settings → Online Accounts. Uses D-Bus app activation so it works
/// both natively and inside a Flatpak (with `--talk-name=org.gnome.Settings`);
/// falls back to the CLI on non-GNOME/older setups.
fn open_online_accounts() {
    if activate_online_accounts_panel().is_err() {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("online-accounts")
            .spawn();
    }
}

fn activate_online_accounts_panel() -> Result<(), gtk::glib::Error> {
    let conn = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)?;
    // org.freedesktop.Application.ActivateAction(action: s, parameter: av, a{sv}).
    // GNOME Settings' "launch-panel" action takes a (sav): (panel_id, extra_args).
    let panel = ("online-accounts", Vec::<gtk::glib::Variant>::new()).to_variant();
    let params: Vec<gtk::glib::Variant> = vec![panel];
    let platform: std::collections::HashMap<String, gtk::glib::Variant> =
        std::collections::HashMap::new();
    let args = ("launch-panel", params, platform).to_variant();
    conn.call_sync(
        Some("org.gnome.Settings"),
        "/org/gnome/Settings",
        "org.freedesktop.Application",
        "ActivateAction",
        Some(&args),
        None,
        gtk::gio::DBusCallFlags::NONE,
        -1,
        gtk::gio::Cancellable::NONE,
    )?;
    Ok(())
}

/// The brand id for a GNOME Online Accounts provider name ("Google",
/// "Microsoft 365"…): the two mail providers GOA offers, else the blue
/// envelope.
pub(crate) fn brand_for_goa(provider: &str) -> &'static str {
    let p = provider.to_ascii_lowercase();
    if p.contains("google") {
        "gmail"
    } else if p.contains("microsoft") || p.contains("outlook") || p.contains("365") {
        "outlook"
    } else {
        "mail"
    }
}

/// The Provider picker's rows: the provider's mark (or the generic
/// envelope) before its name, in the row and in the list that drops down.
/// Shared with the welcome wizard's picker, which lists a subset by the
/// same labels.
pub(crate) fn provider_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let bx = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::None);
            bx.append(&gtk::Image::new());
            bx.append(&label);
            item.set_child(Some(&bx));
        }
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        // By name, not position: the row's own selected-value slot is a
        // list item with no position.
        let name = item.item().and_downcast::<gtk::StringObject>().map(|o| o.string().to_string()).unwrap_or_default();
        let Some(provider) = PROVIDERS.iter().find(|p| p.label == name) else { return };
        let Some(bx) = item.child().and_downcast::<gtk::Box>() else { return };
        let Some(old) = bx.first_child() else { return };
        let label = old.next_sibling().and_downcast::<gtk::Label>();
        bx.remove(&old);
        bx.prepend(&crate::brand::image_or(provider.brand, 20, crate::brand::GENERIC_MAIL));
        if let Some(label) = label {
            label.set_label(&name);
        }
    });
    factory
}

fn non_ellipsizing_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::None);
            label.set_margin_start(6);
            label.set_margin_end(6);
            item.set_child(Some(&label));
        }
    });
    factory.connect_bind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let text = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|o| o.string())
                .unwrap_or_default();
            if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                label.set_label(&text);
            }
        }
    });
    factory
}

fn display_name(acc: &AccountConfig) -> String {
    if acc.name.trim().is_empty() {
        acc.email.clone()
    } else {
        acc.name.clone()
    }
}

/// Build an `AccountConfig` from the current editor form values.
impl AccountsWindow {
    /// The emoji to save: the editor's, unless the circle is set to show a
    /// picture.
    fn saved_emoji(&self) -> Option<String> {
        if self.picture_mode {
            None
        } else {
            self.emoji.clone().filter(|e| !e.is_empty())
        }
    }

    /// The picture to save: the editor's, when the circle is set to show
    /// one.
    fn saved_avatar(&self) -> Option<String> {
        if self.picture_mode {
            self.avatar.clone()
        } else {
            None
        }
    }

    /// Redraw the editor's preview circle and show the rows of the chosen
    /// mode: the accent colour, and the picture, emoji or initials the
    /// sidebar would show — the initials from the label, else the name,
    /// else the email, as the sidebar derives them.
    /// Everything an account editor holds, as one comparable string: what
    /// Save would write, less the signature — that lives in a WebView and
    /// answers only asynchronously (`RichEditor::is_dirty` covers it).
    fn editor_fingerprint(&self, widgets: &AccountsWindowWidgets) -> String {
        let account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
        format!(
            "{account:?}|{:?}|{:?}|{:?}|{}|{}|{}",
            self.alias_edits,
            self.read_folder_roles(widgets),
            self.read_sent_copy_path(widgets),
            widgets.server_saves_row.is_active(),
            widgets.provider_row.selected(),
            self.pending_oauth_refresh.is_some(),
        )
    }

    /// Whether anything in the open account editor differs from what it was
    /// opened with. Leaving an untouched editor must not ask about saving;
    /// with nothing recorded, assume it was touched rather than risk
    /// dropping an edit.
    fn editor_touched(&self, widgets: &AccountsWindowWidgets) -> bool {
        self.editor_seed
            .as_ref()
            .is_none_or(|seed| *seed != self.editor_fingerprint(widgets))
    }

    fn refresh_preview(&self, widgets: &AccountsWindowWidgets) {
        let color = crate::color::to_hex(&widgets.color_btn.rgba());
        self.preview_css.load_from_string(&format!(
            ".account-preview-disc {{ background-color: {color}; }}"
        ));
        let disc = &widgets.preview_disc;
        while let Some(child) = disc.first_child() {
            disc.remove(&child);
        }
        let initials = || {
            let label = trimmed(&widgets.label_row);
            let name = trimmed(&widgets.name_row);
            let email = trimmed(&widgets.email_row);
            let shown = if !label.is_empty() && label != email {
                label
            } else if name.is_empty() {
                email.clone()
            } else {
                name
            };
            crate::ui::initials::glyph_picture(
                &crate::ui::sidebar::account_initials(&shown, &email),
                &color,
                0.47,
                72,
            )
        };
        let picture = self.saved_avatar().and_then(|n| crate::config::avatar_path(&n));
        let emoji = self.saved_emoji();
        // The Gravatar this address has, when it was asked for and found
        // (#189) — the same order the sidebar circle and the cards use.
        let gravatar = widgets
            .gravatar_row
            .is_active()
            .then(|| crate::avatar::own_gravatar(&trimmed(&widgets.email_row)))
            .flatten();
        let glyph: gtk::Widget = match (&gravatar, &picture, &emoji) {
            (Some(texture), ..) => {
                crate::ui::initials::picture_from_texture(texture, 72).upcast()
            }
            (None, Some(path), _) => crate::ui::initials::avatar_picture(path, 72).upcast(),
            (None, None, Some(em)) => {
                crate::ui::initials::glyph_picture(em, &color, 0.55, 72).upcast()
            }
            _ => initials().upcast(),
        };
        disc.append(&glyph);
        widgets.emoji_row.set_visible(!self.picture_mode);
        widgets.picture_row.set_visible(self.picture_mode);
        widgets.emoji_clear_btn.set_visible(emoji.is_some());
        widgets.avatar_clear_btn.set_visible(picture.is_some());
    }
}

/// The side of the square copy an avatar is stored as: sharp in a 30px
/// disc on a 2x display, small on disk.
const AVATAR_PX: i32 = 256;

/// Copy a chosen image in as an account avatar (#162): oriented by its
/// EXIF tag, scaled to cover a square, centre-cropped and saved as PNG
/// under the avatars directory with a fresh name, which is returned.
fn import_avatar_file(src: &std::path::Path) -> Result<String, String> {
    use gtk::gdk_pixbuf::{InterpType, Pixbuf};
    let dir = crate::config::avatars_dir().ok_or("no data directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let pb = Pixbuf::from_file(src).map_err(|e| e.to_string())?;
    let pb = pb.apply_embedded_orientation().unwrap_or(pb);
    let (w, h) = (pb.width().max(1), pb.height().max(1));
    // Scale so the shorter side is AVATAR_PX (an image smaller than that
    // is left at its size), then cut the longer side down to a square.
    let side = AVATAR_PX.min(w.min(h));
    let (sw, sh) = if w <= h {
        (side, (h as f64 * side as f64 / w as f64).round().max(side as f64) as i32)
    } else {
        ((w as f64 * side as f64 / h as f64).round().max(side as f64) as i32, side)
    };
    let scaled = pb.scale_simple(sw, sh, InterpType::Bilinear).ok_or("could not scale the image")?;
    let square = scaled.new_subpixbuf((sw - side) / 2, (sh - side) / 2, side, side);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("avatar-{stamp}.png");
    square.savev(dir.join(&name), "png", &[]).map_err(|e| e.to_string())?;
    Ok(name)
}

fn read_account(
    widgets: &AccountsWindowWidgets,
    emoji: Option<String>,
    avatar: Option<String>,
) -> AccountConfig {
    let protocol = if widgets.protocol_row.selected() == 1 {
        Protocol::Pop3
    } else {
        Protocol::Imap
    };
    let default_port = if protocol == Protocol::Pop3 { 995 } else { 993 };
    AccountConfig {
        name: trimmed(&widgets.name_row),
        email: trimmed(&widgets.email_row),
        protocol,
        imap_host: trimmed(&widgets.host_row),
        imap_port: trimmed(&widgets.port_row).parse().unwrap_or(default_port),
        smtp_host: trimmed(&widgets.smtp_row),
        smtp_port: trimmed(&widgets.smtp_port_row).parse().unwrap_or(587),
        username: trimmed(&widgets.user_row),
        password: widgets.pass_row.text().to_string(),
        smtp_separate: widgets.smtp_separate_row.is_active(),
        smtp_username: trimmed(&widgets.smtp_user_row),
        smtp_password: widgets.smtp_pass_row.text().to_string(),
        color: Some(crate::color::to_hex(&widgets.color_btn.rgba())),
        emoji,
        avatar,
        gravatar: widgets.gravatar_row.is_active(),
        // Filled in by SaveWithSig from the rich-text editor.
        signature: None,
        signature_html: true,
        // Only store a custom label; blank or same-as-email falls back to email.
        label: {
            let l = trimmed(&widgets.label_row);
            let email = trimmed(&widgets.email_row);
            if l.is_empty() || l == email {
                None
            } else {
                Some(l)
            }
        },
        // The alias list is model state (alias_edits), assigned by SaveWithSig.
        aliases: Vec::new(),
        // Defaults for a new account; preserved from the original when editing.
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
        push: match widgets.push_row.selected() {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        },
        // Assigned by SaveWithSig from the Special Folders combos.
        folder_roles: Default::default(),
        sent_copy_path: None,
        server_saves_sent: false,
        empty_junk_days: AUTO_EMPTY_DAYS
            .get(widgets.empty_junk_row.selected() as usize)
            .copied()
            .unwrap_or(0),
        empty_trash_days: AUTO_EMPTY_DAYS
            .get(widgets.empty_trash_row.selected() as usize)
            .copied()
            .unwrap_or(0),
        // Row 0 is Automatic; the rest follow PGP_KEY_CHOICES.
        pgp_key: PGP_KEY_CHOICES.with(|c| {
            let sel = widgets.pgp_key_row.selected() as usize;
            sel.checked_sub(1).and_then(|i| c.borrow().get(i).map(|k| k.fingerprint.clone()))
        }),
    }
}

thread_local! {
    /// The user's own keys as the editor's OpenPGP combo lists them (#133),
    /// filled when the editor opens and read when it saves. The combo shows
    /// labels; this keeps the fingerprints behind them.
    static PGP_KEY_CHOICES: std::cell::RefCell<Vec<crate::pgp::KeyInfo>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Fill the OpenPGP key combo with the user's own keys and select the
/// account's, or Automatic.
fn fill_pgp_key_row(widgets: &AccountsWindowWidgets, chosen: Option<&str>) {
    let keys: Vec<crate::pgp::KeyInfo> = if crate::pgp::available() {
        crate::pgp::list_keys(&crate::pgp::Gpg::system(), true)
            .into_iter()
            .filter(|k| k.usable() && k.can_sign)
            .collect()
    } else {
        Vec::new()
    };
    let mut labels = vec![i18n("Automatic")];
    labels.extend(keys.iter().map(|k| format!("{} ({})", k.primary_uid(), crate::pgp::key_display(&k.key_id))));
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    widgets.pgp_key_row.set_model(Some(&gtk::StringList::new(&refs)));
    widgets.pgp_key_row.set_list_factory(Some(&non_ellipsizing_factory()));
    let selected = chosen
        .and_then(|f| keys.iter().position(|k| k.fingerprint.eq_ignore_ascii_case(f)))
        .map(|i| i as u32 + 1)
        .unwrap_or(0);
    widgets.pgp_key_row.set_selected(selected);
    PGP_KEY_CHOICES.with(|c| *c.borrow_mut() = keys);
}

/// Auto-empty choices (#140), in combo order: never, then the ages.
const AUTO_EMPTY_DAYS: &[u32] = &[0, 7, 14, 30];

/// The combo index for a stored age (an unknown value shows as "Never").
fn auto_empty_index(days: u32) -> u32 {
    AUTO_EMPTY_DAYS.iter().position(|d| *d == days).unwrap_or(0) as u32
}

/// The combo's labels, translated.
fn auto_empty_labels() -> Vec<String> {
    AUTO_EMPTY_DAYS
        .iter()
        .map(|d| {
            if *d == 0 {
                i18n("Never")
            } else {
                crate::i18n::ni18n_f("After {n} day", "After {n} days", *d, &[("n", &d.to_string())])
            }
        })
        .collect()
}

/// Whether the editor's HTML is effectively empty (no visible content).
fn signature_is_empty(html: &str) -> bool {
    let stripped: String = {
        let mut out = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    stripped.replace("&nbsp;", " ").trim().is_empty()
}

/// Left-justify every wrapping label under `root`. Works around a libadwaita
/// 1.9 quirk where an `AdwPreferencesGroup` description renders fill-justified
/// (stretched word gaps) whenever its text is shorter than the label's width;
/// row titles and other single-line labels don't wrap, so they're left alone.
fn left_justify_wrapping_labels(root: &gtk::Widget) {
    if let Some(label) = root.downcast_ref::<gtk::Label>() {
        if label.wraps() {
            label.set_justify(gtk::Justification::Left);
        }
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        left_justify_wrapping_labels(&c);
        child = c.next_sibling();
    }
}

/// Put the editor page into the navigation view if it isn't there (it is
/// left out until first needed — see the panel's init).
fn mount_editor(widgets: &AccountsWindowWidgets) {
    if widgets.editor_page.parent().is_none() {
        widgets.nav.add(&widgets.editor_page);
    }
}

fn fill_editor(widgets: &AccountsWindowWidgets, acc: &AccountConfig) {
    widgets.name_row.set_text(&acc.name);
    widgets.email_row.set_text(&acc.email);
    // Reflect the account's provider in the dropdown (OAuth by endpoint, known
    // password providers by server, otherwise "IMAP/POP3 Account").
    widgets.provider_row.set_selected(provider_index_for_account(acc));
    widgets
        .protocol_row
        .set_selected(if acc.protocol == Protocol::Pop3 { 1 } else { 0 });
    widgets.host_row.set_text(&acc.imap_host);
    widgets.port_row.set_text(&acc.imap_port.to_string());
    widgets.smtp_row.set_text(&acc.smtp_host);
    widgets.smtp_port_row.set_text(&acc.smtp_port.to_string());
    widgets.user_row.set_text(&acc.username);
    widgets.pass_row.set_text(&acc.password);
    widgets.smtp_separate_row.set_active(acc.smtp_separate);
    widgets.smtp_user_row.set_text(&acc.smtp_username);
    widgets.smtp_pass_row.set_text(&acc.smtp_password);
    widgets.push_row.set_selected(match acc.push {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    });
    widgets.empty_junk_row.set_selected(auto_empty_index(acc.empty_junk_days));
    widgets.empty_trash_row.set_selected(auto_empty_index(acc.empty_trash_days));
    fill_pgp_key_row(widgets, acc.pgp_key.as_deref());
    // Show the effective label (custom, or the email address).
    widgets
        .label_row
        .set_text(acc.label.as_deref().unwrap_or(&acc.email));

    // OAuth client detail fields. GOA accounts (goa_id set) can't be
    // re-authenticated here, so they show as password (their mechanism is kept on
    // save); natively-added OAuth accounts show their client details.
    let s = (acc.goa_id.is_none() && acc.oauth)
        .then_some(acc.oauth_settings.as_ref())
        .flatten();
    widgets.oauth_client_id_row.set_text(s.map(|s| s.client_id.as_str()).unwrap_or(""));
    widgets.oauth_secret_row.set_text(s.map(|s| s.client_secret.as_str()).unwrap_or(""));
    widgets.oauth_auth_url_row.set_text(s.map(|s| s.auth_url.as_str()).unwrap_or(""));
    widgets.oauth_token_url_row.set_text(s.map(|s| s.token_url.as_str()).unwrap_or(""));
    widgets.oauth_scope_row.set_text(s.map(|s| s.scopes.as_str()).unwrap_or(""));
    widgets.oauth_status.set_visible(false);
    widgets.oauth_signin_btn.set_sensitive(true);

    // Signature is loaded into the rich-text editor by the caller.
    widgets.test_result.set_visible(false);
    widgets.test_btn.set_sensitive(true);
}

/// Grey out everything GNOME Online Accounts owns.
///
/// An account imported from GOA takes its address, servers, protocol and
/// credentials from the system; editing them here would either be overwritten
/// the next time GOA is read, or quietly disagree with what the rest of the
/// desktop uses. What stays editable is what Hylki owns: the sender's display
/// name, signature, colour, emoji and label.
fn set_connection_editable(widgets: &AccountsWindowWidgets, editable: bool) {
    for row in [
        widgets.email_row.upcast_ref::<gtk::Widget>(),
        widgets.host_row.upcast_ref(),
        widgets.port_row.upcast_ref(),
        widgets.smtp_row.upcast_ref(),
        widgets.smtp_port_row.upcast_ref(),
        widgets.user_row.upcast_ref(),
        widgets.pass_row.upcast_ref(),
        widgets.smtp_user_row.upcast_ref(),
        widgets.smtp_pass_row.upcast_ref(),
    ] {
        row.set_sensitive(editable);
    }
    widgets.provider_row.set_sensitive(editable);
    widgets.protocol_row.set_sensitive(editable);
    widgets.smtp_separate_row.set_sensitive(editable);
    // OAuth client details belong to a natively-added account; a GOA one gets its
    // tokens from the system.
    for row in [
        widgets.oauth_client_id_row.upcast_ref::<gtk::Widget>(),
        widgets.oauth_secret_row.upcast_ref(),
        widgets.oauth_auth_url_row.upcast_ref(),
        widgets.oauth_token_url_row.upcast_ref(),
        widgets.oauth_scope_row.upcast_ref(),
    ] {
        row.set_sensitive(editable);
    }
    widgets.oauth_signin_btn.set_sensitive(editable);
    // Testing stays available: it is read-only, and confirming that the imported
    // settings actually connect is exactly what someone would want here.
}

fn clear_editor(widgets: &AccountsWindowWidgets) {
    widgets.name_row.set_text("");
    widgets.email_row.set_text("");
    widgets.provider_row.set_selected(manual_index());
    widgets.protocol_row.set_selected(0);
    widgets.host_row.set_text("");
    widgets.port_row.set_text("993");
    widgets.smtp_row.set_text("");
    widgets.smtp_port_row.set_text("587");
    widgets.user_row.set_text("");
    widgets.pass_row.set_text("");
    widgets.smtp_separate_row.set_active(false);
    widgets.push_row.set_selected(0);
    widgets.empty_junk_row.set_selected(0);
    widgets.empty_trash_row.set_selected(0);
    fill_pgp_key_row(widgets, None);
    widgets.smtp_user_row.set_text("");
    widgets.smtp_pass_row.set_text("");
    widgets.label_row.set_text("");
    widgets.oauth_client_id_row.set_text("");
    widgets.oauth_secret_row.set_text("");
    widgets.oauth_auth_url_row.set_text("");
    widgets.oauth_token_url_row.set_text("");
    widgets.oauth_scope_row.set_text("");
    widgets.oauth_status.set_visible(false);
    widgets.test_result.set_visible(false);
    widgets.test_btn.set_sensitive(true);
}

fn trimmed(row: &impl IsA<gtk::Editable>) -> String {
    row.text().trim().to_string()
}

/// Dropdown index of the first provider entry of a given kind (the manual entry
/// if none — shouldn't happen for kinds present in the table).
fn kind_index(kind: ProviderKind) -> u32 {
    PROVIDERS
        .iter()
        .position(|p| p.kind == kind)
        .map(|i| i as u32)
        .unwrap_or_else(manual_index)
}

/// Dropdown index of the `Preset` provider whose incoming server matches `host`,
/// or the manual entry when nothing matches.
fn preset_index_for_host(host: &str) -> u32 {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return manual_index();
    }
    PROVIDERS
        .iter()
        .position(|p| p.kind == ProviderKind::Preset && p.imap_host.eq_ignore_ascii_case(&host))
        .map(|i| i as u32)
        .unwrap_or_else(manual_index)
}

/// Dropdown index reflecting an existing account: its OAuth provider (by token
/// endpoint) for native OAuth accounts, otherwise the matching password provider.
fn provider_index_for_account(acc: &AccountConfig) -> u32 {
    if acc.goa_id.is_none() && acc.oauth {
        let kind = match acc.oauth_settings.as_ref() {
            Some(s) if s.token_url.contains("googleapis") => ProviderKind::Google,
            Some(s) if s.token_url.contains("microsoftonline") => ProviderKind::Microsoft,
            _ => ProviderKind::CustomOAuth,
        };
        return kind_index(kind);
    }
    preset_index_for_host(&acc.imap_host)
}

/// The brand id of the service an existing account is on: Microsoft 365
/// over Graph, an OAuth account by its token endpoint, otherwise by its
/// incoming server (the well-known hosts, then the provider table, whose
/// manual entry gives anything else the blue envelope).
fn brand_for_account(acc: &AccountConfig) -> &'static str {
    if acc.protocol == Protocol::Graph {
        return "outlook";
    }
    if let Some(s) = acc.oauth_settings.as_ref().filter(|_| acc.oauth) {
        if s.token_url.contains("googleapis") {
            return "gmail";
        }
        if s.token_url.contains("microsoftonline") {
            return "outlook";
        }
    }
    // Native OAuth against anything else: the custom-OAuth envelope.
    if acc.oauth && acc.goa_id.is_none() {
        return "mail-oauth";
    }
    let host = acc.imap_host.trim().to_ascii_lowercase();
    if host.contains("gmail") || host.contains("googlemail") {
        return "gmail";
    }
    if host.contains("outlook") || host.contains("office365") || host.contains("hotmail") || host.contains("live.com") {
        return "outlook";
    }
    if host.contains("proton") {
        return "proton";
    }
    provider_at(preset_index_for_host(&host)).brand
}

/// What the account list's provider mark says on hover: the provider's
/// name as the picker lists it, without the picker's own hints ("sign in",
/// "Bridge"); a plain IMAP/POP3 account names its server instead.
fn provider_name(brand: &str, acc: &AccountConfig) -> String {
    match brand {
        "gmail" => i18n("Google (Gmail)"),
        "outlook" => i18n("Microsoft 365 / Outlook"),
        "proton" => i18n("Proton Mail"),
        "mail-oauth" => i18n("Custom OAuth"),
        "mail" => {
            let host = acc.imap_host.trim();
            if host.is_empty() {
                i18n("IMAP/POP3 account")
            } else {
                i18n_f("IMAP/POP3 account on {host}", &[("host", host)])
            }
        }
        other => PROVIDERS
            .iter()
            .find(|p| p.brand == other)
            .map(|p| i18n(p.label))
            .unwrap_or_else(|| i18n("Mail account")),
    }
}

fn parse_color(hex: &str) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::parse(hex).unwrap_or_else(|_| gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 1.0))
}

#[cfg(test)]
mod tests {
    use super::{manual_index, preset_index_for_host, provider_at, ProviderKind, PROVIDERS};

    #[test]
    fn known_host_maps_to_its_own_entry() {
        let idx = preset_index_for_host("imap.mail.me.com");
        assert_eq!(provider_at(idx).label, "iCloud");
        // Case-insensitive.
        assert_eq!(preset_index_for_host("IMAP.FASTMAIL.COM"), preset_index_for_host("imap.fastmail.com"));
        assert_eq!(provider_at(preset_index_for_host("imap.fastmail.com")).label, "Fastmail");
    }

    #[test]
    fn unknown_or_empty_host_falls_back_to_manual() {
        assert_eq!(preset_index_for_host("mail.example.org"), manual_index());
        assert_eq!(preset_index_for_host(""), manual_index());
        assert_eq!(preset_index_for_host("  "), manual_index());
        assert_eq!(provider_at(manual_index()).kind, ProviderKind::Manual);
    }

    #[test]
    fn removed_password_providers_are_gone() {
        // Gmail/Hotmail no longer work with a password — only via OAuth.
        for p in PROVIDERS {
            if p.kind == ProviderKind::Preset {
                assert_ne!(p.imap_host, "imap.gmail.com");
                assert_ne!(p.imap_host, "outlook.office365.com");
            }
        }
    }

    #[test]
    fn provider_table_is_well_formed() {
        // Distinct labels; presets have sane servers; exactly one Manual entry.
        let mut labels: Vec<&str> = PROVIDERS.iter().map(|p| p.label).collect();
        let n = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), n, "provider labels must be unique");
        assert_eq!(PROVIDERS.iter().filter(|p| p.kind == ProviderKind::Manual).count(), 1);
        for p in PROVIDERS {
            if p.kind == ProviderKind::Preset {
                assert!(!p.imap_host.is_empty() && !p.smtp_host.is_empty(), "{}", p.label);
                assert!(p.imap_port > 0 && p.smtp_port > 0, "{}", p.label);
                // Each preset's host round-trips to its own entry.
                assert_eq!(provider_at(preset_index_for_host(p.imap_host)).label, p.label);
            }
        }
    }
}

impl AccountsWindow {
    /// Human labels for the filter enums, shared by rows and the dialog.
    fn field_label(f: crate::config::FilterField) -> String {
        use crate::config::FilterField::*;
        i18n(match f {
            FromAddress => "From address",
            FromName => "From name",
            Subject => "Subject",
            Recipients => "To or Cc",
            ReplyTo => "Reply-To address",
            Body => "Message body",
        })
    }
    fn match_label(m: crate::config::FilterMatch) -> String {
        use crate::config::FilterMatch::*;
        i18n(match m {
            Contains => "contains",
            Equals => "is exactly",
            StartsWith => "starts with",
            EndsWith => "ends with",
        })
    }

    /// One condition as the rule rows print it: `Subject contains “a, b”`.
    /// A body condition is always a "contains", whatever it stores (#191).
    fn condition_label(c: &crate::config::FilterCondition) -> String {
        let matcher = if c.field == crate::config::FilterField::Body {
            crate::config::FilterMatch::Contains
        } else {
            c.matcher
        };
        format!("{} {} \u{201c}{}\u{201d}", Self::field_label(c.field), Self::match_label(matcher), c.value)
    }

    /// Re-render the Filters group's rule rows.
    fn rebuild_filter_rows(&self, sender: &ComponentSender<Self>) {
        let Some(list) = &self.filters_list else { return };
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        list.set_visible(!self.filter_rules.is_empty());
        for (i, r) in self.filter_rules.iter().enumerate() {
            let row = adw::ActionRow::new();
            // Roomier than a stock row: the stacked switches and the two-line
            // title were pressed against the row edges.
            row.add_css_class("filter-rule-row");
            // Activating the row opens it in the filter dialog for editing,
            // as the alias rows do.
            row.set_activatable(true);
            let s = sender.clone();
            row.connect_activated(move |_| s.input(AccountsInput::EditFilter(i)));
            // Every condition, joined the way the rule combines them (#192).
            let joiner = if r.any { i18n(" or ") } else { i18n(" and ") };
            let conditions: Vec<String> = r.conditions().iter().map(Self::condition_label).collect();
            let conditions = conditions.join(&joiner);
            // A named rule (#197) is listed by its name, with the conditions
            // moved down beside what it does; an unnamed one reads exactly
            // as it always did.
            let named = !r.name.trim().is_empty();
            row.set_title(if named { r.name.trim() } else { conditions.as_str() });
            // "account → folder", "account, tagged Work", or both (#71).
            let mut subtitle = String::new();
            if named {
                subtitle.push_str(&conditions);
                subtitle.push('\n');
            }
            subtitle.push_str(&r.account_email);
            if !r.dest_path.is_empty() {
                let dest = self
                    .folders_by_email
                    .get(&r.account_email)
                    .and_then(|fs| fs.iter().find(|(p, _)| *p == r.dest_path))
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| r.dest_path.clone());
                subtitle.push_str(&format!(" \u{2192} {dest}"));
            }
            if !r.tag.is_empty() {
                let name = self
                    .tags
                    .iter()
                    .find(|t| t.keyword.eq_ignore_ascii_case(&r.tag))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| r.tag.clone());
                subtitle.push_str(&i18n_f(", tagged {tag}", &[("tag", &name)]));
            }
            row.set_subtitle(&subtitle);
            // A named rule's subtitle runs to two lines (the conditions,
            // then where the mail goes); an unnamed one keeps the one it had.
            row.set_subtitle_lines(if named { 0 } else { 1 });
            // The trash button removes; a chevron says the row opens the
            // rule's editor, as the account and cloud rows do. The rule's
            // "count unread" switch lives in the editor.
            let rm = gtk::Button::from_icon_name("co.hyprlab.Hylki-user-trash-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            rm.set_tooltip_text(Some(i18n("Remove filter").as_str()));
            let s = sender.clone();
            rm.connect_clicked(move |_| s.input(AccountsInput::RemoveFilter(i)));
            row.add_suffix(&rm);
            let next = gtk::Image::from_icon_name("co.hyprlab.Hylki-go-next-symbolic");
            next.add_css_class("dim-label");
            row.add_suffix(&next);
            list.append(&row);
        }
    }

    /// The Apply Now button while a filter run is under way (#198): a turning
    /// spinner beside a label saying so. A press meanwhile starts nothing.
    fn set_filters_applying(&mut self, on: bool) {
        self.filters_applying = on;
        if let Some(spinner) = &self.apply_filters_spinner {
            spinner.set_visible(on);
            spinner.set_spinning(on);
        }
        if let Some(label) = &self.apply_filters_label {
            label.set_label(&if on { i18n("Applying…") } else { i18n("Apply Now") });
        }
    }

    /// The tag finder's button while a scan runs: a turning spinner beside
    /// a label saying so. A press meanwhile starts nothing.
    fn set_tag_scanning(&mut self, on: bool) {
        self.tag_scanning = on;
        if let Some(spinner) = &self.find_tags_spinner {
            spinner.set_visible(on);
            spinner.set_spinning(on);
        }
        if let Some(label) = &self.find_tags_label {
            label.set_label(&if on { i18n("Searching…") } else { i18n("Find Tags…") });
        }
    }

    /// The tag finder's report: every keyword found that is not a tag here
    /// yet, each with a proposed name and colour, ticked to be imported.
    /// "Import All" takes the lot; "Import Selected" only the ticked ones.
    fn show_tag_findings(
        &self,
        parent: Option<&gtk::Window>,
        found: Vec<TagProposal>,
        sender: &ComponentSender<Self>,
    ) {
        if found.is_empty() {
            let dialog = adw::MessageDialog::new(
                parent,
                Some(i18n("No New Tags").as_str()),
                Some(i18n("No tags were found in your mailboxes that are not already set up here.").as_str()),
            );
            dialog.add_response("ok", &i18n("OK"));
            dialog.set_default_response(Some("ok"));
            dialog.set_close_response("ok");
            dialog.present();
            return;
        }

        let n = found.len();
        let dialog = adw::MessageDialog::new(
            parent,
            Some(i18n("Tags Found").as_str()),
            Some(
                i18n_f(
                    "{n} tags are in use in your mailboxes but not set up here. \
                     Import them all, or choose which to add. Names and colours \
                     can be changed afterwards.",
                    &[("n", &n.to_string())],
                )
                .as_str(),
            ),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("selected", &i18n("Import Selected"));
        dialog.add_response("all", &i18n("Import All"));
        dialog.set_response_appearance("all", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("all"));
        dialog.set_close_response("cancel");

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let mut checks: Vec<(gtk::CheckButton, crate::config::Tag)> = Vec::new();
        for p in &found {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&p.tag.name));
            let mut parts = vec![p.tag.keyword.clone()];
            if p.count > 0 {
                parts.push(i18n_f("{n} messages", &[("n", &p.count.to_string())]));
            }
            parts.extend(p.places.iter().cloned());
            row.set_subtitle(&gtk::glib::markup_escape_text(&parts.join(" · ")));
            row.add_prefix(&crate::ui::context_menu::swatch_widget(&p.tag.color, true));
            let check = gtk::CheckButton::new();
            check.set_active(true);
            check.set_valign(gtk::Align::Center);
            row.add_suffix(&check);
            row.set_activatable_widget(Some(&check));
            checks.push((check, p.tag.clone()));
            list.append(&row);
        }
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_propagate_natural_height(true);
        scroller.set_max_content_height(360);
        // Wider than the dialog's default column, so a row's keyword, count
        // and folders read on a line or two.
        scroller.set_size_request(520, -1);
        scroller.set_child(Some(&list));
        dialog.set_extra_child(Some(&scroller));

        // The report is the showcase's to capture (HYLKI_SHOWCASE_FIND_TAGS).
        if std::env::var("HYLKI_SHOWCASE_FIND_TAGS").is_ok() {
            if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                let d = dialog.clone();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    crate::app::showcase_capture(d.upcast_ref(), &path);
                });
            }
        }

        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            let tags: Vec<crate::config::Tag> = match resp {
                "all" => checks.iter().map(|(_, t)| t.clone()).collect(),
                "selected" => checks
                    .iter()
                    .filter(|(c, _)| c.is_active())
                    .map(|(_, t)| t.clone())
                    .collect(),
                _ => return,
            };
            if !tags.is_empty() {
                s.input(AccountsInput::ImportTags(tags));
            }
        });
        dialog.present();
    }

    /// The Tags list (#71): a row per tag — its colour as a disc, its name,
    /// its keyword — activating to edit, with a remove button.
    fn rebuild_tag_rows(&self, sender: &ComponentSender<Self>) {
        let Some(list) = &self.tags_list else { return };
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        list.set_visible(!self.tags.is_empty());
        for (i, t) in self.tags.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            let s = sender.clone();
            row.connect_activated(move |_| s.input(AccountsInput::EditTag(i)));
            row.set_title(&gtk::glib::markup_escape_text(&t.name));
            row.set_subtitle(&gtk::glib::markup_escape_text(&t.keyword));
            let handle = gtk::Image::from_icon_name("co.hyprlab.Hylki-list-drag-handle-symbolic");
            handle.add_css_class("dim-label");
            row.add_prefix(&handle);
            row.add_prefix(&crate::ui::context_menu::swatch_widget(&t.color, true));
            // The number that toggles it from the keyboard (#157), for the
            // first nine.
            if i < 9 {
                let key = gtk::Label::new(Some(&(i + 1).to_string()));
                key.add_css_class("shortcut-key");
                key.set_valign(gtk::Align::Center);
                key.set_tooltip_text(Some(
                    i18n("Press this key on a message to add or remove the tag (with single-key shortcuts on)").as_str(),
                ));
                row.add_suffix(&key);
            }
            // Drag to reorder, as the accounts list does.
            let drag = gtk::DragSource::new();
            drag.set_actions(gtk::gdk::DragAction::MOVE);
            let from = i as u32;
            drag.connect_prepare(move |_, _, _| {
                Some(gtk::gdk::ContentProvider::for_value(&from.to_value()))
            });
            row.add_controller(drag);
            let drop = gtk::DropTarget::new(gtk::glib::Type::U32, gtk::gdk::DragAction::MOVE);
            let to = i;
            let input = sender.input_sender().clone();
            drop.connect_drop(move |_, value, _, _| {
                if let Ok(from) = value.get::<u32>() {
                    let _ = input.send(AccountsInput::MoveTag { from: from as usize, to });
                    true
                } else {
                    false
                }
            });
            row.add_controller(drop);
            let rm = gtk::Button::from_icon_name("co.hyprlab.Hylki-user-trash-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            rm.set_tooltip_text(Some(i18n("Remove tag").as_str()));
            let s = sender.clone();
            rm.connect_clicked(move |_| s.input(AccountsInput::RemoveTag(i)));
            row.add_suffix(&rm);
            // A chevron says the row opens the tag's editor, as the
            // account and cloud rows do.
            let next = gtk::Image::from_icon_name("co.hyprlab.Hylki-go-next-symbolic");
            next.add_css_class("dim-label");
            row.add_suffix(&next);
            list.append(&row);
        }
    }

    /// The tag dialog (#71): name, colour, keyword. The keyword follows the
    /// name until the user edits it; editing an existing tag keeps its
    /// keyword unless changed, since the messages carry the keyword, not
    /// the name.
    fn open_tag_page(
        &self,
        nav: &adw::NavigationView,
        sender: &ComponentSender<Self>,
        edit: Option<(usize, crate::config::Tag)>,
    ) {
        use crate::config::{Tag, TAG_COLORS};
        let parent = relm4::main_application().active_window();
        let (title, verb) = if edit.is_some() {
            (i18n("Edit Tag"), i18n("Save"))
        } else {
            (i18n("Add Tag"), i18n("Add Tag"))
        };

        let form = gtk::ListBox::new();
        form.add_css_class("boxed-list");
        form.set_selection_mode(gtk::SelectionMode::None);

        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));

        let keyword_row = adw::EntryRow::new();
        keyword_row.set_title(&i18n("Keyword"));
        keyword_row.set_tooltip_text(Some(
            i18n("How the tag is stored on the server: letters, digits and most punctuation, \
                  no spaces. Thunderbird's built-in tags are $label1 to $label5.")
                .as_str(),
        ));

        // The keyword follows the name until it is typed into by hand. The
        // follow is done under a guard: set_text empties the field before
        // refilling it, and each step fires `changed`, so without the guard
        // the empty step read as hand-editing after the second letter.
        let keyword_touched = std::rc::Rc::new(std::cell::Cell::new(edit.is_some()));
        let syncing = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let keyword_row = keyword_row.clone();
            let touched = keyword_touched.clone();
            let syncing = syncing.clone();
            name_row.connect_changed(move |row| {
                if !touched.get() {
                    syncing.set(true);
                    keyword_row.set_text(&Tag::keyword_for(&row.text()));
                    syncing.set(false);
                }
            });
        }
        {
            let touched = keyword_touched.clone();
            let syncing = syncing.clone();
            keyword_row.connect_changed(move |_| {
                if !syncing.get() {
                    touched.set(true);
                }
            });
        }

        // The colour: one disc per palette entry on a line of its own under
        // a caption (beside a title, eight discs left the title wrapping one
        // letter per line in the dialog's width). Toggle buttons in one
        // group behave as radios; the pressed one is the choice.
        let color_row = gtk::ListBoxRow::new();
        color_row.set_activatable(false);
        color_row.set_selectable(false);
        let color_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        color_box.set_margin_top(10);
        color_box.set_margin_bottom(10);
        color_box.set_margin_start(12);
        color_box.set_margin_end(12);
        let color_label = gtk::Label::new(Some(i18n("Colour").as_str()));
        color_label.set_halign(gtk::Align::Start);
        color_label.add_css_class("caption");
        color_label.add_css_class("dim-label");
        color_box.append(&color_label);
        let swatches = gtk::FlowBox::new();
        swatches.set_selection_mode(gtk::SelectionMode::None);
        swatches.set_homogeneous(true);
        // Nine discs (the palette and the custom one) as two even rows.
        swatches.set_min_children_per_line(5);
        swatches.set_max_children_per_line(5);
        swatches.set_column_spacing(4);
        swatches.set_row_spacing(4);
        swatches.set_halign(gtk::Align::Start);
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(
            edit.as_ref().map(|(_, t)| t.color.clone()).unwrap_or_else(|| TAG_COLORS[0].to_string()),
        ));
        let mut first: Option<gtk::ToggleButton> = None;
        for color in TAG_COLORS {
            let toggle = gtk::ToggleButton::new();
            toggle.add_css_class("flat");
            toggle.add_css_class("circular");
            toggle.set_child(Some(&crate::ui::context_menu::swatch_widget(color, true)));
            toggle.set_tooltip_text(Some(color));
            if let Some(f) = &first {
                toggle.set_group(Some(f));
            } else {
                first = Some(toggle.clone());
            }
            toggle.set_active(chosen.borrow().eq_ignore_ascii_case(color));
            let chosen = chosen.clone();
            let color = color.to_string();
            toggle.connect_toggled(move |t| {
                if t.is_active() {
                    *chosen.borrow_mut() = color.clone();
                }
            });
            swatches.insert(&toggle, -1);
        }
        // A ninth disc for any colour (#147): a hue wheel until one is picked,
        // then the picked colour. Pressing it opens the GTK colour chooser;
        // it stays a member of the toggle group so a palette pick clears it.
        // A tag edited with a colour outside the palette opens on this disc.
        {
            let custom: Option<String> = edit
                .as_ref()
                .map(|(_, t)| t.color.clone())
                .filter(|c| !TAG_COLORS.iter().any(|p| p.eq_ignore_ascii_case(c)));
            let toggle = gtk::ToggleButton::new();
            toggle.add_css_class("flat");
            toggle.add_css_class("circular");
            toggle.set_child(Some(&custom_swatch_widget(custom.as_deref())));
            toggle.set_tooltip_text(Some(i18n("Custom colour…").as_str()));
            if let Some(f) = &first {
                toggle.set_group(Some(f));
            }
            toggle.set_active(custom.is_some());
            let custom = std::rc::Rc::new(std::cell::RefCell::new(custom));
            let chosen = chosen.clone();
            let parent = parent.clone();
            toggle.connect_clicked(move |t| {
                let picker = gtk::ColorDialog::new();
                picker.set_with_alpha(false);
                picker.set_title(&i18n("Tag Colour"));
                let initial = gtk::gdk::RGBA::parse(chosen.borrow().as_str())
                    .unwrap_or(gtk::gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));
                let t = t.clone();
                let chosen = chosen.clone();
                let custom = custom.clone();
                picker.choose_rgba(
                    parent.as_ref(),
                    Some(&initial),
                    None::<&gtk::gio::Cancellable>,
                    move |res| match res {
                        Ok(rgba) => {
                            let hex = format!(
                                "#{:02x}{:02x}{:02x}",
                                (rgba.red() * 255.0).round() as u8,
                                (rgba.green() * 255.0).round() as u8,
                                (rgba.blue() * 255.0).round() as u8,
                            );
                            t.set_child(Some(&custom_swatch_widget(Some(&hex))));
                            *custom.borrow_mut() = Some(hex.clone());
                            *chosen.borrow_mut() = hex;
                            t.set_active(true);
                        }
                        Err(_) => {
                            // Cancelled: back to whatever the choice was. A
                            // palette colour lives in its own disc; a custom
                            // one picked earlier stays on this disc.
                            if custom.borrow().is_none() {
                                let back = chosen.borrow().clone();
                                let mut child = t.parent().and_then(|p| p.parent()).and_then(|b| b.first_child());
                                while let Some(c) = child {
                                    if let Some(tb) = c.first_child().and_downcast::<gtk::ToggleButton>() {
                                        if tb.tooltip_text().is_some_and(|tip| tip.eq_ignore_ascii_case(&back)) {
                                            tb.set_active(true);
                                            break;
                                        }
                                    }
                                    child = c.next_sibling();
                                }
                            }
                        }
                    },
                );
            });
            swatches.insert(&toggle, -1);
        }
        color_box.append(&swatches);
        color_row.set_child(Some(&color_box));

        let edit_index = edit.as_ref().map(|(i, _)| *i);
        if let Some((_, tag)) = &edit {
            name_row.set_text(&tag.name);
            keyword_row.set_text(&tag.keyword);
        }

        form.append(&name_row);
        form.append(&color_row);
        form.append(&keyword_row);

        let s = sender.clone();
        self.push_form_page(nav, "tag", &title, &verb, &form, move || {
            let name = name_row.text().trim().to_string();
            if name.is_empty() {
                return false;
            }
            // A keyword the server would refuse falls back to the derived one.
            let typed = keyword_row.text().trim().to_string();
            let mut keyword = if Tag::valid_keyword(&typed) { typed } else { Tag::keyword_for(&name) };
            if keyword.is_empty() {
                keyword = "Tag".to_string();
            }
            let tag = Tag { name, keyword, color: chosen.borrow().clone() };
            s.input(match edit_index {
                Some(i) => AccountsInput::TagEdited(i, tag),
                None => AccountsInput::TagAdded(tag),
            });
            true
        });
    }

    /// Slide a form in as its own page, the way the account editor does:
    /// its header carries the window's close button and a Save button;
    /// the back button (or a swipe) leaves without saving. `save` reads
    /// the form and returns true once it went through, which pops the page.
    fn push_form_page(
        &self,
        nav: &adw::NavigationView,
        tag: &str,
        title: &str,
        verb: &str,
        form: &gtk::ListBox,
        save: impl Fn() -> bool + 'static,
    ) {
        let group = adw::PreferencesGroup::new();
        group.add(form);
        let page = adw::PreferencesPage::new();
        page.add(&group);
        // Enter in an entry saves, like the dialog's default response did.
        let mut entries = Vec::new();
        let mut child = form.first_child();
        while let Some(c) = child {
            if let Some(entry) = c.downcast_ref::<adw::EntryRow>() {
                entries.push(entry.clone());
            }
            child = c.next_sibling();
        }
        self.push_form_content(nav, tag, title, verb, page.upcast_ref(), entries, save);
    }

    /// [`push_form_page`] for any content widget: the filter editor builds
    /// its own page of groups. `entries` are the entry rows whose Enter
    /// should save.
    fn push_form_content(
        &self,
        nav: &adw::NavigationView,
        tag: &str,
        title: &str,
        verb: &str,
        content: &gtk::Widget,
        entries: Vec<adw::EntryRow>,
        save: impl Fn() -> bool + 'static,
    ) {
        let save: std::rc::Rc<dyn Fn() -> bool> = std::rc::Rc::new(save);
        let header = adw::HeaderBar::new();
        header.set_show_end_title_buttons(true);
        let save_btn = gtk::Button::with_label(verb);
        save_btn.add_css_class("suggested-action");
        {
            let nav = nav.clone();
            let save = save.clone();
            save_btn.connect_clicked(move |_| {
                if save() {
                    nav.pop();
                }
            });
        }
        header.pack_end(&save_btn);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(content));

        let nav_page = adw::NavigationPage::new(&view, title);
        nav_page.set_tag(Some(tag));
        for entry in entries {
            let nav = nav.clone();
            let save = save.clone();
            entry.connect_entry_activated(move |_| {
                if save() {
                    nav.pop();
                }
            });
        }
        *self.form_save.borrow_mut() = Some(Box::new(move || save()));
        nav.push(&nav_page);
    }

    /// The filter editor, a page of groups: the account; one group per
    /// condition (#192), each its own Where/Match/Text set with a remove
    /// button in its header; Add Condition and the all/any chooser; and
    /// "Then", what a match does. With `edit`, it opens on that existing
    /// rule (prefilled, "Save") and replaces it in place; otherwise it adds.
    fn open_filter_page(
        &self,
        nav: &adw::NavigationView,
        sender: &ComponentSender<Self>,
        edit: Option<(usize, crate::config::FilterRule)>,
    ) {
        use crate::config::{FilterField, FilterMatch, FilterRule};
        let emails: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        if emails.is_empty() {
            return;
        }
        let (title, verb) = if edit.is_some() {
            (i18n("Edit Filter"), i18n("Save"))
        } else {
            (i18n("Add Filter"), i18n("Add Filter"))
        };

        // The page: groups stacked the way an AdwPreferencesPage stacks
        // them, in a box of our own so condition groups can slot in
        // between the fixed ones.
        let page_box = gtk::Box::new(gtk::Orientation::Vertical, 24);
        page_box.set_margin_top(24);
        page_box.set_margin_bottom(24);
        page_box.set_margin_start(12);
        page_box.set_margin_end(12);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(600);
        clamp.set_tightening_threshold(400);
        clamp.set_child(Some(&page_box));
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scrolled.set_vexpand(true);
        scrolled.set_child(Some(&clamp));

        let account_group = adw::PreferencesGroup::new();
        // A name of your own for the rule (#197): what the filters list
        // calls it, in place of spelling its conditions out. Optional.
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name (optional)"));
        account_group.add(&name_row);
        let account_row = adw::ComboRow::new();
        account_row.set_title(&i18n("Account"));
        let email_refs: Vec<&str> = emails.iter().map(|s| s.as_str()).collect();
        account_row.set_model(Some(&gtk::StringList::new(&email_refs)));
        account_group.add(&account_row);

        // The conditions (#192): each a titled group of Where/Match/Text
        // rows with a remove button in its header (hidden while it is the
        // only one). Body conditions (#191) pin the matcher to "contains",
        // the only search a server offers, and say so.
        let conds: std::rc::Rc<std::cell::RefCell<Vec<CondRows>>> = Default::default();
        let field_names: Vec<String> = FilterField::ALL.iter().map(|f| Self::field_label(*f)).collect();
        let match_names: Vec<String> = FilterMatch::ALL.iter().map(|m| Self::match_label(*m)).collect();
        let make_cond = {
            let field_names = field_names.clone();
            let match_names = match_names.clone();
            move |init: Option<&crate::config::FilterCondition>| -> CondRows {
                let group = adw::PreferencesGroup::new();
                let remove = gtk::Button::from_icon_name("co.hyprlab.Hylki-user-trash-symbolic");
                remove.add_css_class("flat");
                remove.set_valign(gtk::Align::Center);
                remove.set_tooltip_text(Some(i18n("Remove condition").as_str()));
                group.set_header_suffix(Some(&remove));
                let field = adw::ComboRow::new();
                field.set_title(&i18n("Where"));
                let refs: Vec<&str> = field_names.iter().map(|s| s.as_str()).collect();
                field.set_model(Some(&gtk::StringList::new(&refs)));
                let matcher = adw::ComboRow::new();
                matcher.set_title(&i18n("Match"));
                matcher.set_subtitle(&i18n("Commas separate alternatives; any one of them counts"));
                let refs: Vec<&str> = match_names.iter().map(|s| s.as_str()).collect();
                matcher.set_model(Some(&gtk::StringList::new(&refs)));
                let value = adw::EntryRow::new();
                value.set_title(&i18n("Text to match"));
                if let Some(c) = init {
                    if let Some(i) = FilterField::ALL.iter().position(|f| *f == c.field) {
                        field.set_selected(i as u32);
                    }
                    if let Some(i) = FilterMatch::ALL.iter().position(|m| *m == c.matcher) {
                        matcher.set_selected(i as u32);
                    }
                    value.set_text(&c.value);
                }
                let explain = {
                    let matcher = matcher.clone();
                    move |field: &adw::ComboRow| {
                        let chosen = FilterField::ALL.get(field.selected() as usize).copied();
                        let body = chosen == Some(FilterField::Body);
                        if body {
                            matcher.set_selected(0);
                        }
                        matcher.set_sensitive(!body);
                        field.set_subtitle(&match chosen {
                            Some(FilterField::Body) => {
                                i18n("Searched on the server; nothing is downloaded")
                            }
                            Some(FilterField::ReplyTo) => {
                                i18n("From address when no Reply-To is set")
                            }
                            _ => String::new(),
                        });
                    }
                };
                explain(&field);
                field.connect_selected_notify(explain);
                group.add(&field);
                group.add(&matcher);
                group.add(&value);
                CondRows { group, remove, field, matcher, value }
            }
        };

        let add_group = adw::PreferencesGroup::new();
        let add_row = adw::ActionRow::new();
        add_row.set_title(&i18n("Add Condition"));
        add_row.set_activatable(true);
        add_row.add_prefix(&gtk::Image::from_icon_name("co.hyprlab.Hylki-list-add-symbolic"));
        // All or any (#192); only shown once there is a second condition.
        let combine_row = adw::ComboRow::new();
        combine_row.set_title(&i18n("Condition matching"));
        combine_row.set_model(Some(&gtk::StringList::new(&[
            i18n("All must match").as_str(),
            i18n("Any one may match").as_str(),
        ])));
        add_group.add(&add_row);
        // In a group of its own, below Add Condition; hidden with its row.
        let combine_group = adw::PreferencesGroup::new();
        combine_group.add(&combine_row);
        combine_group.set_visible(false);

        // Number the groups, title the later Where rows after the combining
        // rule ("And where" / "Or where"), and show the remove buttons and
        // the chooser once there is more than one condition.
        let relabel = {
            let conds = conds.clone();
            let combine_row = combine_row.clone();
            let combine_group = combine_group.clone();
            move || {
                let conds = conds.borrow();
                let any = combine_row.selected() == 1;
                let several = conds.len() > 1;
                for (i, c) in conds.iter().enumerate() {
                    c.group.set_title(&i18n_f("Condition {n}", &[("n", &(i + 1).to_string())]));
                    c.field.set_title(&if i == 0 {
                        i18n("Where")
                    } else if any {
                        i18n("Or where")
                    } else {
                        i18n("And where")
                    });
                    c.remove.set_visible(several);
                }
                combine_group.set_visible(several);
            }
        };
        {
            let relabel = relabel.clone();
            combine_row.connect_selected_notify(move |_| relabel());
        }
        // Add a condition group below the ones there are, above the Add
        // Condition group.
        let add_cond = {
            let conds = conds.clone();
            let page_box = page_box.clone();
            let account_group = account_group.clone();
            let relabel = relabel.clone();
            let make_cond = make_cond.clone();
            let form_save = self.form_save.clone();
            let nav = nav.clone();
            move |init: Option<&crate::config::FilterCondition>| {
                let c = make_cond(init);
                let after: gtk::Widget = conds
                    .borrow()
                    .last()
                    .map(|last| last.group.clone().upcast())
                    .unwrap_or_else(|| account_group.clone().upcast());
                page_box.insert_child_after(&c.group, Some(&after));
                {
                    let conds = conds.clone();
                    let page_box = page_box.clone();
                    let relabel = relabel.clone();
                    let group = c.group.clone();
                    c.remove.connect_clicked(move |_| {
                        let gone = {
                            let mut conds = conds.borrow_mut();
                            if conds.len() < 2 {
                                return;
                            }
                            conds.iter().position(|c| c.group == group).map(|i| conds.remove(i))
                        };
                        if let Some(gone) = gone {
                            page_box.remove(&gone.group);
                            relabel();
                        }
                    });
                }
                // Enter in the text saves, as in every form page; the save
                // closure is looked up when pressed, so a condition added
                // before the page is up gets it too.
                {
                    let form_save = form_save.clone();
                    let nav = nav.clone();
                    c.value.connect_entry_activated(move |_| {
                        let saved = form_save.borrow().as_ref().is_some_and(|save| save());
                        if saved {
                            nav.pop();
                        }
                    });
                }
                conds.borrow_mut().push(c);
                relabel();
            }
        };
        {
            let add_cond = add_cond.clone();
            add_row.connect_activated(move |_| add_cond(None));
        }

        // Then: what a match does.
        let action_group = adw::PreferencesGroup::new();
        action_group.set_title(&i18n("Then"));
        let dest_row = adw::ComboRow::new();
        dest_row.set_title(&i18n("Move to"));
        dest_row.set_subtitle(&i18n("Leave in Inbox to only tag matching mail"));
        // The destination list follows the chosen account.
        let folders = std::rc::Rc::new(self.folders_by_email.clone());
        let emails_rc = std::rc::Rc::new(emails.clone());
        let dest_paths = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let fill_dest = {
            let dest_row = dest_row.clone();
            let folders = folders.clone();
            let emails_rc = emails_rc.clone();
            let dest_paths = dest_paths.clone();
            move |idx: usize| {
                let empty = Vec::new();
                let list =
                    emails_rc.get(idx).and_then(|e| folders.get(e)).unwrap_or(&empty);
                // Entry 0 files nowhere (a tag-only rule, #71): its path is "".
                let stay = i18n("Leave in Inbox");
                let mut names: Vec<&str> = vec![stay.as_str()];
                names.extend(list.iter().map(|(_, n)| n.as_str()));
                dest_row.set_model(Some(&gtk::StringList::new(&names)));
                let mut paths = vec![String::new()];
                paths.extend(list.iter().map(|(p, _)| p.clone()));
                *dest_paths.borrow_mut() = paths;
            }
        };
        fill_dest(0);
        {
            let fill_dest = fill_dest.clone();
            account_row.connect_selected_notify(move |row| fill_dest(row.selected() as usize));
        }

        // Tag with (#71): none, or one of the tags.
        let tag_row = adw::ComboRow::new();
        tag_row.set_title(&i18n("Tag with"));
        let tags = self.tags.clone();
        {
            let none = i18n("None");
            let mut names: Vec<&str> = vec![none.as_str()];
            names.extend(tags.iter().map(|t| t.name.as_str()));
            tag_row.set_model(Some(&gtk::StringList::new(&names)));
        }

        // Filed mail is still new mail: counted by default, so the unread
        // total stays true to what sits unread; a rule filing newsletters
        // can opt its folder out (#116).
        let count_row = adw::SwitchRow::new();
        count_row.set_title(&i18n("Count unread mail"));
        count_row.set_subtitle(&i18n("Include the folder's unread mail in the tray icon's unread count"));
        count_row.set_active(true);
        action_group.add(&dest_row);
        action_group.add(&tag_row);
        action_group.add(&count_row);

        // Editing: every field starts from the rule as it stands.
        let edit_index = edit.as_ref().map(|(i, _)| *i);
        if let Some((_, rule)) = &edit {
            name_row.set_text(&rule.name);
            if let Some(idx) = emails
                .iter()
                .position(|e| e.eq_ignore_ascii_case(&rule.account_email))
            {
                account_row.set_selected(idx as u32);
                fill_dest(idx);
            }
            combine_row.set_selected(if rule.any { 1 } else { 0 });
            if let Some(idx) = dest_paths.borrow().iter().position(|p| *p == rule.dest_path) {
                dest_row.set_selected(idx as u32);
            }
            if let Some(idx) = tags.iter().position(|t| t.keyword.eq_ignore_ascii_case(&rule.tag)) {
                tag_row.set_selected(idx as u32 + 1);
            }
            count_row.set_active(rule.count_unread);
        }

        page_box.append(&account_group);
        page_box.append(&add_group);
        page_box.append(&combine_group);
        page_box.append(&action_group);
        // The condition groups slot in above Add Condition: the rule's own
        // when editing, one blank one otherwise.
        match &edit {
            Some((_, rule)) => {
                for c in rule.conditions() {
                    add_cond(Some(&c));
                }
            }
            None => add_cond(None),
        }

        // Showcase hook: HYLKI_SHOWCASE_OPEN_ROW=dest|tag|where|match|account
        // activates that combo row once the page is up and logs whether its
        // popover opened and how many choices it holds, the way a click would
        // (the "Move to" list was found empty this way, 2026-09-14).
        if let Ok(which) = std::env::var("HYLKI_SHOWCASE_OPEN_ROW") {
            let row: adw::ComboRow = match which.as_str() {
                "dest" => dest_row.clone(),
                "tag" => tag_row.clone(),
                "where" => conds.borrow()[0].field.clone(),
                "match" => conds.borrow()[0].matcher.clone(),
                _ => account_row.clone(),
            };
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
                gtk::prelude::WidgetExt::activate(&row);
                gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
                    fn find_popover(w: &gtk::Widget) -> Option<gtk::Popover> {
                        if let Some(p) = w.downcast_ref::<gtk::Popover>() {
                            return Some(p.clone());
                        }
                        let mut c = w.first_child();
                        while let Some(child) = c {
                            if let Some(p) = find_popover(&child) {
                                return Some(p);
                            }
                            c = child.next_sibling();
                        }
                        None
                    }
                    tracing::info!(
                        "showcase: {which} row choices={} popover visible={:?}",
                        row.model().map(|m| m.n_items()).unwrap_or(0),
                        find_popover(row.upcast_ref()).map(|p| p.is_visible()),
                    );
                });
            });
        }
        // HYLKI_SHOWCASE_ADD_CONDITION=<n> adds that many blank conditions
        // once the page is up, for a capture of the stacked groups.
        if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ADD_CONDITION").ok().map(|v| v.parse::<usize>()) {
            let add_cond = add_cond.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
                for _ in 0..n {
                    add_cond(None);
                }
            });
        }

        let s = sender.clone();
        self.push_form_content(nav, "filter", &title, &verb, scrolled.upcast_ref(), vec![name_row.clone()], move || {
            // Every condition with text; a blank extra one is dropped. The
            // matcher of a body condition is whatever the pinned row says.
            let conditions: Vec<crate::config::FilterCondition> = conds
                .borrow()
                .iter()
                .filter_map(|c| {
                    let value = c.value.text().trim().to_string();
                    if value.is_empty() {
                        return None;
                    }
                    Some(crate::config::FilterCondition {
                        field: FilterField::ALL[c.field.selected() as usize % FilterField::ALL.len()],
                        matcher: FilterMatch::ALL[c.matcher.selected() as usize % FilterMatch::ALL.len()],
                        value,
                    })
                })
                .collect();
            let paths = dest_paths.borrow();
            let (Some(email), Some(dest)) = (
                emails.get(account_row.selected() as usize),
                paths.get(dest_row.selected() as usize),
            ) else {
                return false;
            };
            let tag = match tag_row.selected() {
                0 => String::new(),
                i => tags.get(i as usize - 1).map(|t| t.keyword.clone()).unwrap_or_default(),
            };
            // A rule needs something to match and something to do.
            if conditions.is_empty() || (dest.is_empty() && tag.is_empty()) {
                return false;
            }
            let mut rule = FilterRule {
                name: name_row.text().trim().to_string(),
                account_email: email.clone(),
                field: FilterField::FromAddress,
                matcher: FilterMatch::Contains,
                value: String::new(),
                more: Vec::new(),
                any: combine_row.selected() == 1,
                dest_path: dest.clone(),
                tag,
                count_unread: count_row.is_active(),
            };
            rule.set_conditions(conditions);
            s.input(match edit_index {
                Some(i) => AccountsInput::FilterEdited(i, rule),
                None => AccountsInput::FilterAdded(rule),
            });
            true
        });
    }
}

/// One condition of the filter editor (#192): its own titled group with a
/// remove button in the header, holding the Where, Match and Text rows.
struct CondRows {
    group: adw::PreferencesGroup,
    remove: gtk::Button,
    field: adw::ComboRow,
    matcher: adw::ComboRow,
    value: adw::EntryRow,
}

/// The tag dialog's "any colour" disc (#147): a hue wheel while no custom
/// colour is chosen, the chosen colour as a filled disc once one is.
fn custom_swatch_widget(color: Option<&str>) -> gtk::DrawingArea {
    if let Some(c) = color {
        return crate::ui::context_menu::swatch_widget(c, true);
    }
    let area = gtk::DrawingArea::new();
    area.set_content_width(16);
    area.set_content_height(16);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(|_, cr, w, h| {
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let steps = 12;
        for i in 0..steps {
            let a0 = i as f64 / steps as f64 * std::f64::consts::TAU;
            let a1 = (i + 1) as f64 / steps as f64 * std::f64::consts::TAU;
            let (r, g, b) = hue_rgb(i as f64 / steps as f64);
            cr.set_source_rgb(r, g, b);
            cr.move_to(cx, cy);
            cr.arc(cx, cy, 6.0, a0 - 0.02, a1 + 0.02);
            cr.close_path();
            let _ = cr.fill();
        }
    });
    area
}

/// A fully saturated colour at hue `h` (0..1) as RGB in 0..1.
fn hue_rgb(h: f64) -> (f64, f64, f64) {
    let h6 = h * 6.0;
    let x = 1.0 - ((h6 % 2.0) - 1.0).abs();
    match h6 as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    }
}
