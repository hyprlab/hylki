//! Accounts panel: manage all mail accounts (add / edit / remove / reorder).
//!
//! Not a window of its own: the panel is embedded behind the "Accounts" tab
//! of the combined Accounts & Preferences window (see `ui/preferences.rs`).
//! It uses an `AdwNavigationView` with two pages: a list of accounts (drag rows
//! to set the sidebar order) and a reusable editor form pushed on top.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{
    split_identity, AccountConfig, AliasConfig, NamedSignature, OAuthSettings, Protocol,
};
use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::ui::rich_editor::{self, RichEditor};
use crate::ui::sender_row::{SenderRow, SenderRowOutput};
use crate::worker::{self, ConnTest};

mod account_list;
mod action_editor;
mod address_suggest;
mod aliases;
mod condition_editor;
mod editor;
mod fields;
mod filters;
mod form;
mod form_page;
mod local;
mod messages;
mod navigation;
mod oauth;
mod pages;
mod providers;
mod save;
mod senders;
mod setup;
mod signatures;
mod smart_folders;
mod tags;

use account_list::importable_goa_accounts;
use aliases::AliasDialog;
pub(crate) use editor::AccountSecrets;
pub(crate) use form::non_ellipsizing_factory;
use form::{
    apply_protocol, clear_editor, fill_editor, form_protocol, import_avatar_file, mount_editor,
    parse_color, read_account, set_connection_editable, show_quota, trimmed,
};
pub(crate) use messages::{AccountsCmd, AccountsInit, AccountsInput, AccountsOutput, TagProposal};
pub(crate) use oauth::open_online_accounts;
use pages::{AccountsPage, EditorForm, FiltersPage, SendersPage, SmartPage, TagsPage};
use providers::provider_at;
pub(crate) use providers::{brand_for_goa, provider_factory, Provider, PROVIDERS};
use setup::{connect_editor_open, fill_editor_choices, prepare_editor_page};

const DEFAULT_COLOR: &str = "#3584e4";

/// The open form page's Save (see `AccountsWindow::form_save`).
type FormSave = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn() -> bool>>>>;

pub(crate) struct AccountsWindow {
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
    /// The On This Computer group (#552), shown while there is no such
    /// account to add it as.
    local_group: Option<adw::PreferencesGroup>,
    /// The filters page's search text, read by the list's filter function.
    filters_query: std::rc::Rc<std::cell::RefCell<String>>,
    /// The open filter or tag page's Save: reads the form and hands the
    /// result back, true when it went through (the page then pops). The
    /// settings window's leave-editor prompt saves through it too.
    form_save: FormSave,
    /// The senders page's search text, shared by both lists.
    senders_query: std::rc::Rc<std::cell::RefCell<String>>,
    /// Tags (#71), managed on this tab too — the rules' "Tag with" names them.
    tags: Vec<crate::config::Tag>,
    tags_list: Option<gtk::ListBox>,
    /// Smart folders (#417): saved searches the sidebar lists.
    smart_folders: Vec<crate::config::SmartFolder>,
    smart_list: Option<gtk::ListBox>,
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
    /// The preview circle's color, as a stylesheet on the display.
    preview_css: gtk::CssProvider,
    /// The account list's per-row disc colors (`.acct-list-color-N`),
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
    /// The signature's rich editor — a WebKit view, so it is created when
    /// an account's editor first opens rather than with the panel.
    sig_editor: Option<RichEditor>,
    /// "Edit HTML" / "Preview" at the end of the signature editor's
    /// toolbar, put there when the editor is made.
    sig_source_btn: gtk::Button,
    /// The account's signatures as edited (#445). The one shown is in
    /// `sig_editor` and is read back into its entry when another is shown
    /// or the account is saved.
    sig_edits: Vec<NamedSignature>,
    /// Index into `sig_edits` of the signature in the editor.
    sig_shown: usize,
    /// Which signature new messages start with.
    sig_default: Option<usize>,
    /// A signature was added, deleted, renamed, made the default, or had
    /// its text changed before another was shown: the WebView's own dirty
    /// flag only knows about the one it holds now.
    sig_touched: bool,
    /// The shown signature is being read back: the editor still holds it,
    /// so another switch waits for that to land.
    sig_reading: bool,
    /// The email value the label field currently mirrors, so the label auto-fills
    /// from the email until the user customizes it.
    label_synced: String,
    /// GNOME Online Accounts mail accounts available to import (not yet in Hylki).
    goa: Vec<crate::services::goa::GoaMailAccount>,
    /// Refresh token captured from a successful OAuth sign-in, applied on save.
    pending_oauth_refresh: Option<String>,
    /// Counts the times the editor opened, so a keyring answer for an
    /// editor since replaced is dropped.
    editor_session: u64,
    /// The keyring's answer for the account in the editor, once it came:
    /// Save fills the fields left empty from it.
    editor_secrets: Option<AccountSecrets>,
    /// A keyring read for the editor is out.
    secrets_reading: bool,
    /// Save was pressed before the keyring answered, and runs again when it
    /// does: the keyring may be waiting on its user (#466).
    save_waiting: bool,
    /// The editor's Microsoft sign-in may search the organization's
    /// directory (#410): saved with those scopes, or just approved for them.
    /// Save keeps asking for them, or the next sign-in would drop them.
    directory_granted: bool,
    /// What the directory switch is known to show, so the notify that
    /// filling the editor sets off is not taken for the user turning it on.
    directory_on: bool,
    /// The same two for the sensitivity label names (#411).
    labels_granted: bool,
    labels_on: bool,
    /// The send-as aliases being edited for the account in the editor (#34).
    /// Committed to the account on Save.
    alias_edits: Vec<AliasConfig>,
    /// The open account's identities on its JMAP server (#346), shown
    /// under the aliases and not editable here.
    server_identities: Vec<(String, String)>,
    /// The account's hidden folders (#239) as the editor holds them: the
    /// saved list, less whatever the user has brought back since opening.
    hidden_edits: Vec<String>,
    /// Index into `alias_edits` open in the alias dialog; `None` while adding.
    alias_editing: Option<usize>,
    /// The open alias editor dialog and its fields, if any.
    alias_dialog: Option<AliasDialog>,
}

#[relm4::component(pub(crate))]
impl Component for AccountsWindow {
    type Init = AccountsInit;
    type Input = AccountsInput;
    type Output = AccountsOutput;
    type CommandOutput = AccountsCmd;

    // The panel's pages and the editor's form, built by `pages` before the
    // view and placed below with `#[local_ref]`.
    additional_fields! {
        accounts: AccountsPage,
        tags: TagsPage,
        smart: SmartPage,
        filters: FiltersPage,
        senders: SendersPage,
        form: EditorForm,
    }

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

                            #[local_ref]
                            add_named[Some("accounts")] = accounts_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("tags")] = tags_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("smart")] = smart_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("filters")] = filters_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("senders")] = senders_page -> adw::PreferencesPage {},
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
                        #[local_ref]
                        set_content = editor_form -> adw::PreferencesPage {},
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
        let mut model = AccountsWindow::new(init, &sender);

        let senders_box = model.senders.widget();
        let blacklist_box = model.blacklist.widget();
        // An empty boxed list still draws its frame: a stray line under the
        // entry that adds to it (#129). Shown only once it has a row.
        senders_box.set_visible(!model.sender_addrs.is_empty());
        blacklist_box.set_visible(!model.blacklist_addrs.is_empty());
        let accounts = AccountsPage::build(&sender);
        let accounts_page = &accounts.page;
        let tags = TagsPage::build(&sender);
        let tags_page = &tags.page;
        let smart = SmartPage::build(&sender);
        let smart_page = &smart.page;
        let filters = FiltersPage::build(&sender);
        let filters_page = &filters.page;
        let senders = SendersPage::build(&sender, senders_box, blacklist_box);
        let senders_page = &senders.page;
        let form = EditorForm::build(&sender);
        let editor_form = &form.page;
        let widgets = view_output!();
        prepare_editor_page(&widgets, &sender);
        tracing::debug!(
            "settings window: accounts view built in {:?}",
            t_init.elapsed()
        );
        model.fill_lists(&widgets, &sender);
        fill_editor_choices(&widgets);
        model.connect_editor(&widgets, &sender);
        connect_editor_open(&widgets, &sender, model.form_save.clone());

        tracing::debug!(
            "settings window: accounts panel init {:?}",
            t_init.elapsed()
        );
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
            AccountsInput::RefreshGoa => self.on_refresh_goa(widgets, sender),
            AccountsInput::CloseEditor => self.on_close_editor(widgets),
            AccountsInput::ReleaseSignatureEditor => self.on_release_signature_editor(widgets),
            AccountsInput::DebugEditLabel(text) => widgets.form.label_row.set_text(&text),
            AccountsInput::DebugProvider(label) => self.on_debug_provider(label, widgets),
            AccountsInput::LeaveRequest(page) => self.on_leave_request(page, widgets, sender),
            AccountsInput::LeaveVerdict { page, touched } => {
                self.on_leave_verdict(page, touched, sender)
            }
            AccountsInput::SaveOpenPage => self.on_save_open_page(widgets, sender),
            AccountsInput::SearchFilters(text) => self.on_search_filters(text, widgets),
            AccountsInput::SearchSenders(text) => self.on_search_senders(text),
            AccountsInput::AddAccount => self.on_add_account(widgets, sender),
            AccountsInput::AddLocal => self.on_add_local(widgets, sender),
            AccountsInput::ImportLocal(mbox) => {
                super::accounts::local::pick_import(mbox, &sender, root)
            }
            AccountsInput::EditAccountByEmail(email) => {
                self.on_edit_account_by_email(email, widgets, sender)
            }
            AccountsInput::EditAccount(i) => self.on_edit_account(i, widgets, sender),
            AccountsInput::EmailChanged => self.on_email_changed(widgets),
            AccountsInput::MoveRow { from, to } => self.on_move_row(from, to, widgets, sender),
            AccountsInput::ToggleEnabled { index, enabled } => {
                self.on_toggle_enabled(index, enabled, widgets, sender)
            }
            AccountsInput::ToggleCurrentEnabled(enabled) => {
                self.on_toggle_current_enabled(enabled, widgets, sender)
            }
            AccountsInput::ImportGoa(index) => self.on_import_goa(index, widgets, sender),
            AccountsInput::SetEmoji(text) => self.on_set_emoji(text, widgets),
            AccountsInput::ClearGlyph => self.on_clear_glyph(widgets),
            AccountsInput::SetServerSavesSent(on) => self.on_set_server_saves_sent(on, widgets),
            AccountsInput::SetFolderSort { email, sort } => {
                self.on_set_folder_sort(email, sort, widgets)
            }
            AccountsInput::SetOwnGravatar(on) => self.on_set_own_gravatar(on, widgets, sender),
            AccountsInput::SetPictureMode(on) => self.on_set_picture_mode(on, widgets),
            AccountsInput::RefreshPreview => self.refresh_preview(widgets),
            AccountsInput::PickAvatar => self.on_pick_avatar(sender, root),
            AccountsInput::SetAvatar(name) => self.on_set_avatar(name, widgets),
            AccountsInput::TestConnection => self.on_test_connection(widgets, sender),
            AccountsInput::ProtocolChanged => self.on_protocol_changed(widgets),
            AccountsInput::ProviderChanged => self.on_provider_changed(widgets),
            AccountsInput::OAuthSignIn => self.on_oauth_sign_in(widgets, sender, root),
            AccountsInput::SetGraphDirectory(on) => {
                self.on_set_graph_directory(on, widgets, sender, root)
            }
            AccountsInput::SetGraphLabels(on) => {
                self.on_set_graph_labels(on, widgets, sender, root)
            }
            AccountsInput::OpenOnlineAccounts => open_online_accounts(),
            AccountsInput::Save => self.on_save(widgets, sender),
            AccountsInput::SetFolderChoices(map) => self.on_set_folder_choices(map),
            AccountsInput::SignatureSourceToggle => {
                self.on_signature_source_toggle(widgets, sender)
            }
            AccountsInput::SignatureToSource(html) => self.on_signature_to_source(html, widgets),
            AccountsInput::SignatureToRich(text) => self.on_signature_to_rich(text, widgets),
            AccountsInput::SignatureImport => self.on_signature_import(sender, root),
            AccountsInput::SignatureImportFile(path) => {
                self.on_signature_import_file(path, widgets, root)
            }
            AccountsInput::SigShow => self.on_sig_show(widgets, sender),
            AccountsInput::SigAdd => self.on_sig_add(widgets, sender),
            AccountsInput::SigDelete => self.on_sig_delete(widgets),
            AccountsInput::SigRename => self.on_sig_rename(widgets),
            AccountsInput::SigDefault => self.on_sig_default(widgets),
            AccountsInput::SigTextTouched(dirty) => self.sig_touched |= dirty,
            AccountsInput::SigStored { html, add } => self.on_sig_stored(html, add, widgets),
            AccountsInput::SaveWithSig(sig_html) => {
                self.on_save_with_sig(sig_html, widgets, sender)
            }
            AccountsInput::RemoveCurrent => self.on_remove_current(sender, root),
            AccountsInput::ConfirmRemove => self.on_confirm_remove(widgets, sender),
            AccountsInput::AliasAdd => self.on_alias_add(widgets, sender, root),
            AccountsInput::AliasEdit(i) => self.on_alias_edit(i, widgets, sender, root),
            AccountsInput::UnhideFolder(i) => self.on_unhide_folder(i, widgets, sender),
            AccountsInput::AliasRemove(i) => self.on_alias_remove(i, widgets, sender),
            AccountsInput::AliasDialogSave => self.on_alias_dialog_save(widgets, sender),
            AccountsInput::AliasDialogTest => self.on_alias_dialog_test(widgets, sender),
            AccountsInput::AliasDialogClosed => self.on_alias_dialog_closed(),
            AccountsInput::AddSenderText(text) => self.on_add_sender_text(text, sender),
            AccountsInput::RemoveSenderRow(addr) => self.on_remove_sender_row(addr, sender),
            AccountsInput::AddBlacklistText(text) => self.on_add_blacklist_text(text, sender),
            AccountsInput::RemoveBlacklistRow(addr) => self.on_remove_blacklist_row(addr, sender),
            AccountsInput::AddFilter => self.on_add_filter(widgets, sender),
            AccountsInput::AddFilterFrom(rule) => self.on_add_filter_from(rule, widgets, sender),
            AccountsInput::ApplyFilters => self.on_apply_filters(sender),
            AccountsInput::FiltersApplying(on) => self.set_filters_applying(on),
            AccountsInput::EditFilter(i) => self.on_edit_filter(i, widgets, sender),
            AccountsInput::FilterEdited(i, rule) => self.on_filter_edited(i, rule, sender),
            AccountsInput::RemoveFilter(i) => self.on_remove_filter(i, sender),
            AccountsInput::FilterAdded(rule) => self.on_filter_added(rule, sender),
            AccountsInput::AddSmartFolder => self.open_smart_page(&widgets.nav, &sender, None),
            AccountsInput::EditSmartFolder(i) => self.on_edit_smart_folder(i, widgets, sender),
            AccountsInput::RemoveSmartFolder(i) => self.on_remove_smart_folder(i, sender),
            AccountsInput::SmartFolderSaved(at, folder) => {
                self.on_smart_folder_saved(at, folder, sender)
            }
            AccountsInput::SetSmartFolders(folders) => self.on_set_smart_folders(folders, sender),
            AccountsInput::AddTag => self.open_tag_page(&widgets.nav, &sender, None),
            AccountsInput::EditTag(i) => self.on_edit_tag(i, widgets, sender),
            AccountsInput::RemoveTag(i) => self.on_remove_tag(i, sender),
            AccountsInput::MoveTag { from, to } => self.on_move_tag(from, to, sender),
            AccountsInput::TagAdded(tag) => self.on_tag_added(tag, sender),
            AccountsInput::TagEdited(i, tag) => self.on_tag_edited(i, tag, sender),
            AccountsInput::FindTags => self.on_find_tags(sender),
            AccountsInput::ServerIdentities { email, identities } => {
                self.on_server_identities(email, identities, widgets, sender)
            }
            AccountsInput::Quota { email, quota } => self.on_quota(email, quota, widgets),
            AccountsInput::TagScanning(on) => self.set_tag_scanning(on),
            AccountsInput::TagFindings(found) => self.on_tag_findings(found, sender, root),
            AccountsInput::ImportTags(tags) => self.on_import_tags(tags, sender),
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
                self.on_own_gravatar_fetched(email, outcome, widgets, sender)
            }
            AccountsCmd::GoaListed(listed) => self.on_goa_listed(listed, widgets, sender),
            AccountsCmd::Secrets(session, secrets) => {
                self.on_secrets_read(session, secrets, widgets, sender)
            }
            AccountsCmd::Test(result) => self.on_connection_tested(result, widgets),
            AccountsCmd::OAuth(result) => self.on_signed_in(result, widgets),
            AccountsCmd::DirectoryConsent(result) => self.on_directory_consent(result, widgets),
            AccountsCmd::LabelsConsent(result) => self.on_labels_consent(result, widgets),
            AccountsCmd::AliasTested(result) => self.on_alias_tested(result),
        }
    }
}
